//! Source-position paging, without archiving a copy of an ordinary source file.
use anyhow::{Context, Result, bail, ensure};
use jcode_tool_types::presentation::{scan_characters, select_prefix};
use jcode_tool_types::{OutputSource, ReadPageReference, ToolOutput};
use serde::{Deserialize, Serialize};
use std::fs::{File, Metadata};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Version {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    change_seconds: i64,
    #[cfg(unix)]
    change_nanos: i64,
}

impl Version {
    fn of(metadata: &Metadata) -> Self {
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            created: metadata.created().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            change_seconds: metadata.ctime(),
            #[cfg(unix)]
            change_nanos: metadata.ctime_nsec(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Point {
    schema: u32,
    source: PathBuf,
    version: Version,
    byte: u64,
    line: u64,
    column: u64,
}

/// Line bounds are inclusive and one-based. A point replaces only the start.
pub struct ReadRequest {
    pub path: PathBuf,
    pub point: Option<String>,
    pub start_line: u64,
    pub end_line: Option<u64>,
    pub target: NonZeroUsize,
}

pub struct SourceReader {
    points: PathBuf,
}

impl SourceReader {
    pub fn new(state_root: &Path) -> Self {
        Self {
            points: state_root.join("execution/read-points"),
        }
    }

    fn save(&self, point: &Point) -> Result<String> {
        crate::storage::ensure_dir(&self.points)?;
        let id = uuid::Uuid::new_v4().simple().to_string();
        crate::storage::write_json_secret(&self.points.join(format!("{id}.json")), point)?;
        Ok(id)
    }

    fn load(&self, id: &str) -> Result<Point> {
        ensure!(
            id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid read point"
        );
        let point: Point = crate::storage::read_json(&self.points.join(format!("{id}.json")))
            .context("Read point is unavailable")?;
        ensure!(point.schema == 1, "Unsupported read-point version");
        Ok(point)
    }

    /// Blocking filesystem work. Async callers run this outside their event loop.
    pub fn read(&self, request: ReadRequest) -> Result<ToolOutput> {
        ensure!(request.start_line > 0, "start_line must be positive");
        let source = request
            .path
            .canonicalize()
            .context("Read source is unavailable")?;
        let file = File::open(&source)?;
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "Read source must be a regular file");
        let version = Version::of(&metadata);
        let mut reader = BufReader::new(file);
        let mut start = if let Some(id) = request.point.as_deref() {
            let point = self.load(id)?;
            ensure!(
                point.source == source,
                "Read point belongs to another source"
            );
            ensure!(
                point.version == version,
                "Stale read point: source changed; start a new read"
            );
            ensure!(
                point.byte <= version.length && point.line > 0,
                "Invalid read point position"
            );
            reader.seek(SeekFrom::Start(point.byte))?;
            point
        } else {
            Point {
                schema: 1,
                source: source.clone(),
                version: version.clone(),
                byte: 0,
                line: 1,
                column: 0,
            }
        };
        if request.point.is_none() {
            // Skip by bounded buffers, not read_line (which allocates whole huge lines).
            while start.line < request.start_line {
                let buffer = reader.fill_buf()?;
                if buffer.is_empty() {
                    break;
                }
                let count = buffer
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(buffer.len(), |i| i + 1);
                let newline = buffer[count - 1] == b'\n';
                start.byte += count as u64;
                if newline {
                    start.line += 1;
                }
                reader.consume(count);
            }
        }
        ensure!(
            request.end_line.is_none_or(|end| end >= start.line),
            "end_line precedes the read position"
        );

        let mut current = start.clone();
        let mut rendered = String::new();
        // One mapping per source character, bounded by the presentation window.
        // A mapping includes the preceding generated label, so a page never ends
        // inside a label or creates a zero-progress continuation.
        let mut positions: Vec<(usize, u64, u64, u64)> = Vec::new();
        let window = scan_characters(request.target);
        let mut characters = 0usize;
        let mut label_needed = true;
        let mut complete = false;
        while characters <= window {
            if request.end_line.is_some_and(|end| current.line > end) {
                complete = true;
                break;
            }
            let Some(character) = read_scalar(&mut reader)? else {
                complete = true;
                break;
            };
            if label_needed {
                let label = if current.column == 0 {
                    format!("{:>5}\t", current.line)
                } else {
                    format!("{:>5}:{}\t", current.line, current.column + 1)
                };
                characters += label.chars().count();
                rendered.push_str(&label);
                label_needed = false;
            }
            rendered.push(character);
            characters += 1;
            current.byte += character.len_utf8() as u64;
            if character == '\n' {
                current.line += 1;
                current.column = 0;
                label_needed = true;
            } else {
                current.column += 1;
            }
            positions.push((rendered.len(), current.byte, current.line, current.column));
        }
        // Both the opened object and name must still identify the captured version.
        ensure!(
            Version::of(&reader.get_ref().metadata()?) == version
                && Version::of(&std::fs::metadata(&source)?) == version,
            "Read source changed during capture; retry from current source"
        );
        let prefix = select_prefix(&rendered, request.target, complete);
        let selected = positions
            .iter()
            .rev()
            .find(|(byte, _, _, _)| *byte <= prefix.bytes);
        let (end_rendered, end_byte, end_line, end_column) = if let Some(position) = selected {
            *position
        } else {
            ensure!(
                rendered.is_empty(),
                "output_size is too small for a line label and source character"
            );
            (0, start.byte, start.line, start.column)
        };
        let finished = complete && end_rendered == rendered.len();
        rendered.truncate(end_rendered);
        let retry_point = match request.point {
            Some(id) => id,
            None => self.save(&start)?,
        };
        let next_point = if finished {
            None
        } else {
            Some(self.save(&Point {
                byte: end_byte,
                line: end_line,
                column: end_column,
                ..start.clone()
            })?)
        };
        let reference = ReadPageReference {
            path: source,
            start_byte: start.byte,
            end_byte,
            start_line: start.line,
            end_line,
            retry_point,
            next_point,
        };
        if let Some(point) = &reference.next_point {
            rendered.push_str(&format!("\n[Read page continues: read_point=\"{point}\". Use the same file_path; end_line is optional.]"));
        }
        let mut output = ToolOutput::new(rendered);
        output.source = OutputSource::ReadPage(reference);
        Ok(output)
    }
}

fn read_scalar(reader: &mut impl Read) -> Result<Option<char>> {
    let mut bytes = [0u8; 4];
    if reader.read(&mut bytes[..1])? == 0 {
        return Ok(None);
    }
    let width = match bytes[0] {
        0..=0x7f => 1,
        0xc2..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf4 => 4,
        _ => bail!("Source contains invalid UTF-8"),
    };
    reader
        .read_exact(&mut bytes[1..width])
        .context("Incomplete UTF-8 source character")?;
    Ok(std::str::from_utf8(&bytes[..width])
        .context("Source contains invalid UTF-8")?
        .chars()
        .next())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(path: &Path, target: usize, point: Option<String>) -> ReadRequest {
        ReadRequest {
            path: path.to_path_buf(),
            point,
            start_line: 1,
            end_line: None,
            target: NonZeroUsize::new(target).unwrap(),
        }
    }

    #[test]
    fn pages_cover_exact_source_ranges_and_resume_from_persisted_points() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("source");
        let text = format!("{}\r\n{}\nfinal", "🙂α".repeat(1000), "x\r\n".repeat(80));
        std::fs::write(&path, &text)?;
        let mut point = None;
        let mut assembled = String::new();
        let mut previous = 0;
        loop {
            let output = SourceReader::new(dir.path()).read(request(&path, 100, point))?;
            let OutputSource::ReadPage(page) = output.source else {
                panic!("source page")
            };
            assert_eq!(page.start_byte, previous);
            assert!(page.end_byte > page.start_byte);
            let body = output
                .output
                .split("\n[Read page continues:")
                .next()
                .unwrap();
            let delivered: String = body
                .split_inclusive('\n')
                .map(|line| line.split_once('\t').expect("generated line label").1)
                .collect();
            assert_eq!(
                delivered,
                text[page.start_byte as usize..page.end_byte as usize]
            );
            assembled.push_str(&delivered);
            previous = page.end_byte;
            point = page.next_point;
            if point.is_none() {
                break;
            }
        }
        assert_eq!(assembled, text);
        assert!(!dir.path().join("execution/outputs").exists());
        Ok(())
    }

    #[test]
    fn changed_replaced_and_cross_source_points_reject() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("source");
        std::fs::write(&path, "x".repeat(100))?;
        let reader = SourceReader::new(dir.path());
        let output = reader.read(request(&path, 20, None))?;
        let OutputSource::ReadPage(page) = output.source else {
            panic!()
        };
        let other = dir.path().join("other");
        std::fs::write(&other, "x".repeat(100))?;
        assert!(
            reader
                .read(request(&other, 20, page.next_point.clone()))
                .is_err()
        );
        std::fs::rename(&other, &path)?;
        assert!(reader.read(request(&path, 20, page.next_point)).is_err());
        Ok(())
    }

    #[test]
    fn upper_range_tiny_budget_and_unchanged_retry() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("source");
        std::fs::write(&path, "first\r\nsecond\nthird")?;
        let reader = SourceReader::new(dir.path());
        assert!(reader.read(request(&path, 1, None)).is_err());
        let mut input = request(&path, 100, None);
        input.start_line = 2;
        input.end_line = Some(2);
        let output = reader.read(input)?;
        assert_eq!(output.output, "    2\tsecond\n");
        let OutputSource::ReadPage(page) = output.source else {
            panic!()
        };
        let mut retry = request(&path, 100, Some(page.retry_point));
        retry.end_line = Some(2);
        assert_eq!(reader.read(retry)?.output, output.output);
        Ok(())
    }
}

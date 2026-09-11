//! Source-position paging, without archiving a copy of an ordinary source file.
use anyhow::{Context, Result, bail, ensure};
use jcode_tool_types::presentation::{scan_characters, select_prefix};
use jcode_tool_types::{OutputSource, ReadPageReference, ToolOutput};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Point {
    schema: u32,
    source: PathBuf,
    version: Version,
    byte: u64,
    line: u64,
    column: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    managed_id: Option<String>,
}

/// Line bounds are inclusive and one-based. A point replaces only the start.
pub struct ReadRequest {
    pub path: PathBuf,
    pub point: Option<String>,
    pub start_line: u64,
    pub end_line: Option<u64>,
    pub target: NonZeroUsize,
    pub stop: Option<jcode_agent_runtime::InterruptSignal>,
}

pub struct SourceReader {
    points: PathBuf,
    root: PathBuf,
}

enum ReadSource {
    File(File),
    Managed(Box<super::managed_read::ManagedRead>),
}
impl ReadSource {
    fn metadata(&self) -> std::io::Result<Metadata> {
        match self {
            Self::File(file) => file.metadata(),
            Self::Managed(file) => file.metadata(),
        }
    }
}
impl Read for ReadSource {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::File(file) => file.read(bytes),
            Self::Managed(file) => file.read(bytes),
        }
    }
}
impl Seek for ReadSource {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        match self {
            Self::File(file) => file.seek(position),
            Self::Managed(file) => file.seek(position),
        }
    }
}

impl SourceReader {
    pub fn is_managed_path(root: &Path, path: &Path) -> bool {
        super::managed_read::ManagedRead::is_path(root, path)
    }
    pub fn new(state_root: &Path) -> Self {
        Self {
            points: state_root.join("execution/read-points"),
            root: state_root.into(),
        }
    }

    fn save(&self, point: &Point) -> Result<String> {
        crate::storage::ensure_dir(&self.points)?;
        ensure!(
            std::fs::symlink_metadata(&self.points)?.is_dir(),
            "Read-point directory changed type"
        );
        let id = format!("{:x}", Sha256::digest(serde_json::to_vec(point)?))[..32].to_string();
        let path = self.points.join(format!("{id}.json"));
        if let Ok(metadata) = std::fs::symlink_metadata(&path) {
            ensure!(
                metadata.is_file() && crate::storage::read_json::<Point>(&path)? == *point,
                "Read-point identity conflicts with retained coordinates"
            );
        } else {
            crate::storage::write_json_secret(&path, point)?;
        }
        Ok(id)
    }

    pub(super) fn output_continuation(&self, path: &Path, prefix: &str) -> Result<String> {
        let managed = super::managed_read::ManagedRead::open(&self.root, path)?
            .context("Retained output is not a managed source")?;
        ensure!(
            prefix.len() as u64 <= managed.length,
            "Presentation exceeds captured output"
        );
        let version = Version {
            length: prefix.len() as u64,
            modified: None,
            created: None,
            #[cfg(unix)]
            device: 0,
            #[cfg(unix)]
            inode: 0,
            #[cfg(unix)]
            change_seconds: 0,
            #[cfg(unix)]
            change_nanos: 0,
        };
        let point = Point {
            schema: 2,
            source: path.to_path_buf(),
            version,
            byte: prefix.len() as u64,
            line: prefix.bytes().filter(|byte| *byte == b'\n').count() as u64 + 1,
            column: prefix
                .rsplit('\n')
                .next()
                .unwrap_or_default()
                .chars()
                .count() as u64,
            managed_id: Some(managed.id),
        };
        self.save(&point)
    }

    fn load(&self, id: &str) -> Result<Point> {
        ensure!(
            id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()),
            "Invalid read point"
        );
        let point: Point = crate::storage::read_json(&self.points.join(format!("{id}.json")))
            .context("Read point is unavailable")?;
        ensure!(
            (point.schema == 1 && point.managed_id.is_none())
                || (point.schema == 2 && point.managed_id.is_some()),
            "Unsupported read-point version"
        );
        Ok(point)
    }

    /// Blocking filesystem work. Async callers run this outside their event loop.
    pub fn read(&self, request: ReadRequest) -> Result<ToolOutput> {
        ensure!(request.start_line > 0, "start_line must be positive");
        let (source, file, managed_id, running, length) = if let Some(managed) =
            super::managed_read::ManagedRead::open_with_stop(
                &self.root,
                &request.path,
                request.stop.as_ref(),
            )? {
            let id = managed.id.clone();
            let running = managed.running;
            let length = managed.length;
            (
                request.path.clone(),
                ReadSource::Managed(Box::new(managed)),
                Some(id),
                running,
                Some(length),
            )
        } else {
            let source = request
                .path
                .canonicalize()
                .context("Read source is unavailable")?;
            let file = File::open(&source)?;
            (source, ReadSource::File(file), None, false, None)
        };
        let metadata = file.metadata()?;
        ensure!(metadata.is_file(), "Read source must be a regular file");
        let mut version = Version::of(&metadata);
        if let Some(length) = length {
            version.length = length;
        }
        let mut reader = BufReader::new(file);
        let mut start = if let Some(id) = request.point.as_deref() {
            let point = self.load(id)?;
            ensure!(
                point.source == source,
                "Read point belongs to another source"
            );
            ensure!(
                point.managed_id == managed_id,
                "Read point belongs to a different managed source"
            );
            if managed_id.is_none() {
                ensure!(
                    point.version == version,
                    "Stale read point: source changed; start a new read"
                );
            }
            ensure!(
                point.byte <= version.length && point.line > 0,
                "Invalid read point position"
            );
            reader.seek(SeekFrom::Start(point.byte))?;
            point
        } else {
            Point {
                schema: if managed_id.is_some() { 2 } else { 1 },
                source: source.clone(),
                version: version.clone(),
                byte: 0,
                line: 1,
                column: 0,
                managed_id: managed_id.clone(),
            }
        };
        if request.point.is_none() {
            // Skip by bounded buffers, not read_line (which allocates whole huge lines).
            while start.line < request.start_line {
                ensure!(
                    !request.stop.as_ref().is_some_and(|stop| stop.is_set()),
                    "Source read cancelled"
                );
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
            if positions.len().is_multiple_of(1024) {
                ensure!(
                    !request.stop.as_ref().is_some_and(|stop| stop.is_set()),
                    "Source read cancelled"
                );
            }
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
        if managed_id.is_none() {
            ensure!(
                Version::of(&reader.get_ref().metadata()?) == version
                    && Version::of(&std::fs::metadata(&source)?) == version,
                "Read source changed during capture; retry from current source"
            );
        }
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
        let next_point = if finished && !running {
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
        if let Some(id) = managed_id {
            output.metadata = Some(
                serde_json::json!({"managed_output":id,"captured_bytes":version.length,"running":running,"advanced":end_byte>start.byte}),
            );
            if running && finished {
                output.output.push_str("\n[End of the committed prefix. Producer is still running; wait for more output before continuing.]");
            }
        }
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
    use crate::execution::{
        Capture, ExecutionStore, Invocation, PreparedInvocation, RunState, StorageConfig,
    };
    use jcode_tool_core::{OutputCapture, OutputStream};

    fn managed_capture(root: &Path) -> Result<(Capture, PathBuf)> {
        let store = ExecutionStore::open(root)?;
        let invocation = Invocation {
            session_id: "reader".into(),
            message_id: "message".into(),
            call_path: vec![uuid::Uuid::new_v4().to_string()],
            tool: "fixture".into(),
            input: serde_json::json!({}),
            working_dir: None,
            received_result_digest: None,
        };
        let PreparedInvocation::New(record) = store.prepare(&invocation, "owner")? else {
            panic!()
        };
        store.start(&record.id, "owner")?;
        let capture = Capture::create(store, record, StorageConfig::default())?;
        let path = capture.reference()?.path;
        Ok((capture, path))
    }

    #[test]
    fn managed_points_survive_append_and_read_only_the_committed_prefix() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (capture, path) = managed_capture(directory.path())?;
        let original = format!("{}\r\n", "α".repeat(300));
        capture.write(OutputStream::Stdout, original.as_bytes())?;
        let reader = SourceReader::new(directory.path());
        let first = reader.read(request(&path, 100, None))?;
        let OutputSource::ReadPage(first_page) = first.source else {
            panic!()
        };
        capture.write(OutputStream::Stdout, b"APPENDED\n")?;
        let mut result = ToolOutput::new("");
        result.source = OutputSource::Retained(capture.reference()?);
        capture.seal(result, RunState::Completed)?;
        let second = reader.read(request(&path, 10_000, first_page.next_point))?;
        let OutputSource::ReadPage(second_page) = second.source else {
            panic!()
        };
        assert_eq!(first_page.end_byte, second_page.start_byte);
        assert_eq!(second_page.end_byte, original.len() as u64 + 9);
        assert!(second.output.contains("APPENDED"));
        assert!(second_page.next_point.is_none());
        Ok(())
    }

    #[test]
    fn managed_points_reject_changed_captured_bytes() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (capture, path) = managed_capture(directory.path())?;
        capture.write(OutputStream::Text, &vec![b'x'; 500])?;
        let mut result = ToolOutput::new("");
        result.source = OutputSource::Retained(capture.reference()?);
        capture.seal(result, RunState::Completed)?;
        let reader = SourceReader::new(directory.path());
        let first = reader.read(request(&path, 100, None))?;
        let OutputSource::ReadPage(page) = first.source else {
            panic!()
        };
        std::fs::write(&path, vec![b'y'; 500])?;
        assert!(reader.read(request(&path, 100, page.next_point)).is_err());
        Ok(())
    }

    #[test]
    fn retained_presentation_exposes_a_stable_exact_read_point() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (capture, path) = managed_capture(directory.path())?;
        let text = format!("{}TAIL", "α".repeat(500));
        capture.write(OutputStream::Text, text.as_bytes())?;
        let id = capture.reference()?.invocation_id;
        let mut output = ToolOutput::new("");
        output.source = OutputSource::Retained(capture.reference()?);
        capture.seal(output, RunState::Completed)?;
        let store = ExecutionStore::open(directory.path())?;
        let record = store.inspect(&id)?.unwrap();
        let target = NonZeroUsize::new(100).unwrap();
        let first = store.result(&record, target)?;
        let replay = store.result(&record, target)?;
        assert_eq!(first.output, replay.output);
        let OutputSource::Retained(reference) = first.source else {
            panic!()
        };
        let page = SourceReader::new(directory.path()).read(request(
            &path,
            1000,
            reference.continuation,
        ))?;
        let OutputSource::ReadPage(reference) = page.source else {
            panic!()
        };
        assert_eq!(reference.start_byte, 200);
        assert_eq!(reference.end_byte, text.len() as u64);
        assert!(page.output.contains("TAIL"));
        Ok(())
    }

    #[test]
    fn cancelled_read_does_not_create_or_advance_a_point() -> Result<()> {
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("source");
        std::fs::write(&path, "source")?;
        let signal = jcode_agent_runtime::InterruptSignal::new();
        signal.fire();
        let mut input = request(&path, 100, None);
        input.stop = Some(signal);
        assert!(SourceReader::new(directory.path()).read(input).is_err());
        assert!(!directory.path().join("execution/read-points").exists());
        Ok(())
    }

    #[test]
    fn legacy_sealed_output_index_import_verifies_digest_and_is_idempotent() -> Result<()> {
        for damaged in [false, true] {
            let directory = tempfile::tempdir()?;
            let (capture, path) = managed_capture(directory.path())?;
            let text = format!("{}TAIL", "α".repeat(50_000));
            capture.write(OutputStream::Text, text.as_bytes())?;
            let id = capture.reference()?.invocation_id;
            let mut output = ToolOutput::new("");
            output.source = OutputSource::Retained(capture.reference()?);
            capture.seal(output, RunState::Completed)?;
            drop(capture);
            let store = ExecutionStore::open(directory.path())?;
            store
                .connection()?
                .execute("DELETE FROM output_chunks WHERE run_id=?1", [&id])?;
            if damaged {
                std::fs::write(&path, vec![b'x'; text.len()])?;
            }
            let reader = SourceReader::new(directory.path());
            let result = reader.read(request(&path, 100, None));
            let chunks = || -> Result<i64> {
                Ok(store.connection()?.query_row(
                    "SELECT COUNT(*) FROM output_chunks WHERE run_id=?1",
                    [&id],
                    |row| row.get(0),
                )?)
            };
            if damaged {
                assert!(result.is_err());
                assert_eq!(chunks()?, 0);
            } else {
                let result = result?;
                assert!(result.output.contains("α"));
                let count = chunks()?;
                assert!(count > 0);
                reader.read(request(&path, 100, None))?;
                assert_eq!(chunks()?, count);
                assert_eq!(std::fs::read_to_string(&path)?, text);
                assert_eq!(
                    std::fs::read_dir(directory.path().join("execution/index-imports"))?.count(),
                    0
                );
            }
        }
        Ok(())
    }

    #[test]
    fn legacy_index_failure_is_retryable_without_partial_publication_or_fabricated_integrity()
    -> Result<()> {
        let directory = tempfile::tempdir()?;
        let (capture, path) = managed_capture(directory.path())?;
        let text = "x".repeat(200_000);
        capture.write(OutputStream::Text, text.as_bytes())?;
        let id = capture.reference()?.invocation_id;
        let mut output = ToolOutput::new("");
        output.source = OutputSource::Retained(capture.reference()?);
        capture.seal(output, RunState::Completed)?;
        drop(capture);
        let store = ExecutionStore::open(directory.path())?;
        store
            .connection()?
            .execute("DELETE FROM output_chunks WHERE run_id=?1", [&id])?;
        store.connection()?.execute_batch("CREATE TRIGGER fail_index_import BEFORE INSERT ON output_chunks WHEN NEW.start_byte>0 BEGIN SELECT RAISE(FAIL,'injected index publication failure'); END;")?;
        let reader = SourceReader::new(directory.path());
        assert!(reader.read(request(&path, 100, None)).is_err());
        let count: i64 = store.connection()?.query_row(
            "SELECT COUNT(*) FROM output_chunks WHERE run_id=?1",
            [&id],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        store
            .connection()?
            .execute_batch("DROP TRIGGER fail_index_import;")?;
        reader.read(request(&path, 100, None))?;
        assert_eq!(std::fs::read_to_string(&path)?, text);
        store
            .connection()?
            .execute("DELETE FROM output_chunks WHERE run_id=?1", [&id])?;
        let manifest = store.inspect(&id)?.unwrap().result_path.unwrap();
        let mut value: serde_json::Value = crate::storage::read_json(&manifest)?;
        value.as_object_mut().unwrap().remove("text_sha256");
        crate::storage::write_json_secret(&manifest, &value)?;
        assert!(reader.read(request(&path, 100, None)).is_err());
        Ok(())
    }

    fn request(path: &Path, target: usize, point: Option<String>) -> ReadRequest {
        ReadRequest {
            path: path.to_path_buf(),
            point,
            start_line: 1,
            end_line: None,
            target: NonZeroUsize::new(target).unwrap(),
            stop: None,
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

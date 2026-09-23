//! Disposable local Git LFS transfer fixture. Serves only fixture-owned bytes on loopback.
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::JoinHandle;
use std::time::Duration;

pub(super) struct LfsTestServer {
    address: SocketAddr,
    objects: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    events: Arc<Mutex<Vec<String>>>,
    stopped: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl LfsTestServer {
    pub(super) fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let objects = Arc::new(Mutex::new(HashMap::new()));
        let events = Arc::new(Mutex::new(Vec::new()));
        let stopped = Arc::new(AtomicBool::new(false));
        let (owned_objects, owned_events, owned_stop) =
            (objects.clone(), events.clone(), stopped.clone());
        let thread = std::thread::spawn(move || {
            while !owned_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream
                            .set_read_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        stream
                            .set_write_timeout(Some(Duration::from_secs(5)))
                            .unwrap();
                        if let Err(error) =
                            respond(&mut stream, address, &owned_objects, &owned_events)
                        {
                            owned_events
                                .lock()
                                .unwrap()
                                .push(format!("fixture error: {error}"));
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(10))
                    }
                    Err(error) => panic!("fixture listener: {error}"),
                }
            }
        });
        Self {
            address,
            objects,
            events,
            stopped,
            thread: Some(thread),
        }
    }

    pub(super) fn url(&self) -> String {
        format!("http://{}/lfs", self.address)
    }
    pub(super) fn events(&self) -> Vec<String> {
        self.events.lock().unwrap().clone()
    }

    pub(super) fn add_repo_objects(&self, root: &Path) {
        let dir = root.join(".git/lfs/objects");
        for a in std::fs::read_dir(&dir).unwrap() {
            for b in std::fs::read_dir(a.unwrap().path()).unwrap() {
                for entry in std::fs::read_dir(b.unwrap().path()).unwrap() {
                    let entry = entry.unwrap();
                    let oid = entry.file_name().to_string_lossy().to_string();
                    if oid.len() == 64 && entry.file_type().unwrap().is_file() {
                        let bytes = std::fs::read(entry.path()).unwrap();
                        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), oid);
                        self.objects.lock().unwrap().insert(oid, bytes);
                    }
                }
            }
        }
    }
}

impl Drop for LfsTestServer {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(self.address);
        if let Some(thread) = self.thread.take() {
            thread.join().unwrap();
        }
    }
}

fn respond(
    stream: &mut TcpStream,
    address: SocketAddr,
    objects: &Mutex<HashMap<String, Vec<u8>>>,
    events: &Mutex<Vec<String>>,
) -> std::io::Result<()> {
    let mut request = Vec::new();
    let mut bytes = [0u8; 4096];
    let headers_end = loop {
        let read = stream.read(&mut bytes)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&bytes[..read]);
        assert!(
            request.len() < 1024 * 1024,
            "fixture HTTP request too large"
        );
        if let Some(end) = request.windows(4).position(|part| part == b"\r\n\r\n") {
            break end + 4;
        }
    };
    let headers = std::str::from_utf8(&request[..headers_end]).unwrap();
    let mut first = headers.lines().next().unwrap().split_ascii_whitespace();
    let method = first.next().unwrap().to_owned();
    let path = first.next().unwrap().to_owned();
    let length: usize = headers
        .lines()
        .find_map(|line| {
            line.to_ascii_lowercase()
                .strip_prefix("content-length:")
                .and_then(|value| value.trim().parse().ok())
        })
        .unwrap_or(0);
    while request.len() - headers_end < length {
        let read = stream.read(&mut bytes)?;
        if read == 0 {
            return Ok(());
        }
        request.extend_from_slice(&bytes[..read]);
    }
    events.lock().unwrap().push(format!("{method} {path}"));
    let (code, mime, response): (u16, &str, Vec<u8>) = if method == "POST"
        && path == "/lfs/objects/batch"
    {
        let body: serde_json::Value =
            serde_json::from_slice(&request[headers_end..headers_end + length]).unwrap();
        assert_eq!(body["operation"], "download");
        let available = objects.lock().unwrap();
        let objects: Vec<_> = body["objects"]
            .as_array()
            .unwrap()
            .iter()
            .map(|object| {
                let oid = object["oid"].as_str().unwrap();
                match available.get(oid) {
                    Some(bytes) => serde_json::json!({"oid": oid, "size": bytes.len(),
                    "actions": {"download": {"href": format!("http://{address}/objects/{oid}")}}}),
                    None => serde_json::json!({"oid": oid, "size": object["size"],
                    "error": {"code": 404, "message": "fixture object unavailable"}}),
                }
            })
            .collect();
        (
            200,
            "application/vnd.git-lfs+json",
            serde_json::to_vec(&serde_json::json!({"transfer":"basic","objects":objects})).unwrap(),
        )
    } else if method == "GET" && path.starts_with("/objects/") {
        let oid = &path["/objects/".len()..];
        match objects.lock().unwrap().get(oid) {
            Some(bytes) => (200, "application/octet-stream", bytes.clone()),
            None => (404, "text/plain", b"missing fixture object".to_vec()),
        }
    } else {
        (404, "text/plain", b"unexpected fixture request".to_vec())
    };
    let headers = format!(
        "HTTP/1.1 {code} OK\r\nContent-Type: {mime}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        response.len()
    );
    stream.write_all(headers.as_bytes())?;
    stream.write_all(&response)?;
    stream.flush()
}

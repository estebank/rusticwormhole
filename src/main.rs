use clap::{Parser, Subcommand};
use rayon::ThreadPoolBuildError;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
// use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::net::SocketAddr;
use std::net::{TcpListener, TcpStream};
use std::os::unix::prelude::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::fs::create_dir_all;
use warp::{Buf, Filter};

type Result<T> = std::result::Result<T, Error>;

/// Magic Wormhole clone
#[derive(Debug, Parser)]
#[clap(version = "0.2")]
struct Opts {
    #[clap(long, default_value = "0.0.0.0:9999")]
    registry: String,
    #[clap(subcommand)]
    flavor: Flavor,
    #[clap(long, default_value = "10485760")] // 1024 * 1024 * 10
    /// Size of the send/receive buffers.
    buf_size: usize,
}

/// All the possible mode of operation of this application
#[derive(Debug, Subcommand)]
enum Flavor {
    /// Send a local file to a registered receiver
    Send {
        /// Your username which will be displayed to the receiver
        username: String,
        /// Username of the receiver
        target: String,
        /// File or directory to be sent
        path: PathBuf,
        #[clap(long, default_value = "8")]
        /// Number of files that are allowed to be sent at the same time
        threadpool_size: usize,
    },
    /// List all the registered receivers
    List,
    /// Setup and register receiver so others in the local network can send you files
    Receive {
        username: String,
        port: usize,
        #[clap(default_value = "received_files")]
        target_dir: PathBuf,
    },
    /// Start centralized receiver registry
    Registry,
}

fn main() -> Result<()> {
    let opts: Opts = Opts::parse();
    match opts.flavor {
        Flavor::Send {
            username,
            target,
            path,
            threadpool_size,
        } => send(
            &username,
            &target,
            path,
            &opts.registry,
            opts.buf_size,
            threadpool_size,
        ),
        Flavor::List => list(&opts.registry),
        Flavor::Receive {
            username,
            port,
            target_dir,
        } => receive(&username, port, target_dir, &opts.registry, opts.buf_size),
        Flavor::Registry => registry(&opts.registry),
    }
}

/// List all the registered receivers.
fn list(registry: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let uri = format!("http://{}", registry).parse()?;
    rt.block_on(async {
        let client = hyper::Client::new();
        let res = client.get(uri).await?;
        let body = hyper::body::aggregate(res).await?;
        let map: Map = serde_json::from_reader(body.reader())?;
        println!("Currently registered users:\n");
        for username in map.0.keys() {
            println!("{username}");
        }
        Ok(())
    })
}

/// Minimal wrapper arround syscall `sendfile`
#[cfg(target_os = "macos")]
fn sendfile(file: impl AsRawFd, stream: impl AsRawFd) -> std::io::Result<usize> {
    let file_ptr = file.as_raw_fd();
    let socket_ptr = stream.as_raw_fd();
    let mut length = 0; // Send all bytes.
    let offset = 0 as libc::off_t;
    let res = unsafe {
        libc::sendfile(
            file_ptr,
            socket_ptr,
            offset,
            &mut length,
            std::ptr::null_mut(),
            0,
        )
    };
    if res == -1 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(res as usize)
    }
}

/// Minimal wrapper arround syscall `sendfile`
#[cfg(any(target_os = "linux", target_os = "android"))]
fn sendfile(file: impl AsRawFd, stream: impl AsRawFd) {
    let file = file.as_raw_fd();
    let socket = socket.as_raw_fd();
    // This is the maximum the Linux kernel will write in a single call.
    let count = 0x7ffff000;
    let mut offset = self.written as libc::off_t;
    let res = unsafe { libc::sendfile(socket, file, &mut offset, count) };
    if res == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(res as usize)
    }
}

/// Send the contents of `path` to `addr`.
fn send_single_file(path: PathBuf, addr: String, username: String, buf_size: usize) -> Result<()> {
    let mut file = File::open(&path)?;

    let mut stream = TcpStream::connect(addr)?;
    let end = file.seek(std::io::SeekFrom::End(0))?;
    let header = format!("{}:{}:{}", username, end, path.display());
    stream.write_all(header.as_bytes())?;

    let _ = file.seek(std::io::SeekFrom::Start(0))?;
    let mut go_ahead = vec![0];
    let _bytes_read = stream.read(&mut go_ahead)?;
    if go_ahead[0] != 1 {
        panic!("rejected");
    }

    if buf_size == 0 {
        match sendfile(file, stream) {
            Ok(_bytes) => println!("[sender] Sent `{}`", path.display()),
            Err(err) => println!("[sender] Could not send `{}`: {err:?}", path.display()),
        }
        return Ok(());
    }

    let mut contents = vec![0; buf_size];
    let mut total = 0;
    loop {
        let n = file.read(&mut contents)?;
        stream.write_all(&contents[0..n])?;
        stream.flush()?;
        total += n;
        if n == 0 {
            break;
        }
    }
    println!("[sender] Sent {total} bytes from `{}`", path.display());
    Ok(())
}

/// Send a local file or directory to a registered receiver.
fn send(
    username: &str,
    target: &str,
    path: PathBuf,
    registry: &str,
    buf_size: usize,
    threadpool_size: usize,
) -> Result<()> {
    if !path.exists() {
        panic!("`{}` doesn't exist", path.display());
    }
    if path.has_root() {
        panic!("Only relative paths are accepted");
    }
    let client = hyper::Client::new();
    let uri: http::Uri = format!("http://{}", registry).parse()?;

    let rt = tokio::runtime::Runtime::new()?;
    let map = rt.block_on(async {
        let res = client.get(uri).await.unwrap();
        let body = hyper::body::aggregate(res).await.unwrap();
        let map: std::result::Result<Map, _> = serde_json::from_reader(body.reader());
        map
    });
    let map = map?;
    drop(rt);

    let paths: Vec<PathBuf> = if path.is_dir() {
        let paths: Vec<PathBuf> = glob::glob(&format!("./{}/*", path.display()))?
            .chain(glob::glob(&format!("./{}/**/*", path.display()))?)
            .filter_map(|p| p.ok())
            .filter(|p| p.is_file())
            .collect();
        println!(
            "[sender] Sending file{}: {}",
            if paths.len() == 1 { "" } else { "s" },
            paths
                .iter()
                .map(|p| format!("`{}`", p.display()))
                .collect::<Vec<_>>()
                .join(", "),
        );
        paths
    } else {
        println!("[sender] Sending file: `{}`", path.display());
        vec![path.clone().into()]
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(threadpool_size)
        .build()?;
    pool.scope(|s| {
        for path in paths {
            let username = username.to_string();
            let addr = map.0[target].clone();
            s.spawn(move |_| {
                send_single_file(path, addr, username, buf_size).unwrap();
            });
        }
    });
    Ok(())
}

/// Set up a receiver service.
fn receive(
    username: &str,
    port: usize,
    target_dir: PathBuf,
    registry: &str,
    buf_size: usize,
) -> Result<()> {
    println!("[receiver] Registering username {username} at {registry} to receive files on {port}");
    let rt = tokio::runtime::Runtime::new()?;
    let uri: http::Uri = format!("http://{registry}/register").parse().unwrap();
    rt.block_on(async {
        let client = hyper::Client::new();
        let post = hyper::Request::post(uri).body(hyper::Body::from(format!(
            r#"{{ "username": "{username}", "port": {port} }}"#
        )))?;
        let _res = client.request(post).await?;
        if !_res.status().is_success() {
            let mut reader = hyper::body::aggregate(_res.into_body()).await?.reader();
            let mut body = String::new();
            reader.read_to_string(&mut body)?;

            return Err(Error::Reception);
        }
        create_dir_all(&target_dir).await?;
        println!(
            "[receiver] Writing received files to `{}`",
            target_dir.display()
        );

        let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
        let pool = rayon::ThreadPoolBuilder::new().num_threads(8).build()?;
        pool.scope(|s| {
            while let Ok((stream, _socket_addr)) = listener.accept() {
                let target_dir = target_dir.clone();
                s.spawn(move |_| {
                    process(stream, target_dir, buf_size).unwrap();
                });
            }
        });
        Ok(())
    })
}

/// Process a single file being received.
fn process(mut stream: TcpStream, mut path: PathBuf, buf_size: usize) -> Result<()> {
    let mut contents = vec![0; if buf_size == 0 { 102400000 } else { buf_size }];
    let n = stream.read(&mut contents)?;
    if n == 0 {
        println!("[receiver] `username` and `path` missing?");
        return Ok(());
    }
    let header = std::str::from_utf8(&contents[..n])?.to_string();
    let header = header.split(':').collect::<Vec<_>>();
    let (username, file_len, file_name) = match &header[..] {
        [username, file_len, file_name] => {
            let file_len: std::result::Result<usize, _> = file_len.parse();
            (username, file_len?, file_name)
        }
        _ => panic!(),
    };
    println!("[receiver] Incoming file `{file_name}` from `{username}` with len {file_len}");
    let _bytes_written = stream.write(&[1])?;

    if file_name.contains('/') {
        let mut path = path.clone();
        path.push(file_name.rsplit_once('/').unwrap().0);
        std::fs::create_dir_all(path)?;
    }
    path.push(file_name);

    // If the file already exists we overwrite it.
    println!("[receiver] Writing `{}`", path.display());
    let mut file = File::create(&path)?;
    let mut total = 0;
    let mut consecutive_zeros = 0;
    loop {
        let n = if buf_size == 0 {
            stream.read(&mut contents)?
        } else {
            stream.read(&mut contents)?
        };
        std::io::copy(&mut &contents[..n], &mut file)?;

        total += n;
        if total == file_len || consecutive_zeros > 1000 {
            break;
        }
        if n == 0 {
            consecutive_zeros += 1;
        }
    }
    println!(
        "[receiver] Wrote total {} of {} to `{}`",
        total,
        file_len,
        path.display()
    );
    Ok(())
}

/// Used only for JSON creation.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Map(HashMap<String, String>);

type S = Arc<Mutex<HashMap<String, String>>>;

trait State {
    fn new() -> Self;
}

impl State for S {
    fn new() -> Self {
        Default::default()
    }
}

#[derive(Deserialize, Serialize, Debug)]
struct Mapping {
    username: String,
    port: u32,
}

fn registry(reg: &str) -> Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    rt.block_on(async {
        let state = S::default();

        // `/` serving JSON with the current mappings
        let root = warp::path::end().map({
            let state = state.clone();
            move || warp::reply::json(&Map(state.lock().unwrap().clone()))
        });

        // `/register` POST handler
        let register = warp::post()
            .and(warp::path("register"))
            .and(warp::body::content_length_limit(1024 * 16))
            .and(warp::body::json())
            .and(warp::addr::remote())
            .map({
                let state = state.clone();
                move |Mapping { username, port }, addr: Option<SocketAddr>| {
                    let ip_addr = addr.unwrap().ip();
                    let target_addr = format!("{ip_addr}:{port}");
                    let r = format!("Registering \"{username}\" at {target_addr} from {addr:?}");
                    println!("[registry] {r}");
                    state.lock().unwrap().insert(username, target_addr);
                    r
                }
            });
        let routes = register.or(root);
        println!("[registry] Starting registry listening at `{reg}`");
        let reg: std::net::SocketAddr = reg.parse()?;
        warp::serve(routes).run(reg).await;
        Ok(())
    })
}

#[derive(Debug)]
enum Error {
    Io(std::io::Error),
    Utf8(std::str::Utf8Error),
    InvalidUri(http::uri::InvalidUri),
    AddrParseError(std::net::AddrParseError),
    Any(Box<dyn std::error::Error + Send + Sync>),
    Reception,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{}", err),
            Self::Utf8(err) => write!(f, "{}", err),
            Self::InvalidUri(err) => write!(f, "{}", err),
            Self::AddrParseError(err) => write!(f, "{}", err),
            Self::Any(err) => write!(f, "{}", err),
            Self::Reception => write!(f, "couldn't receive file"),
        }
    }
}

unsafe impl Send for Error {}
unsafe impl Sync for Error {}

impl std::error::Error for Error {}

impl From<Box<dyn std::error::Error + Send + Sync>> for Error {
    fn from(e: Box<dyn std::error::Error + Send + Sync>) -> Self {
        Self::Any(e)
    }
}
impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<std::str::Utf8Error> for Error {
    fn from(e: std::str::Utf8Error) -> Self {
        Self::Utf8(e)
    }
}
impl From<std::net::AddrParseError> for Error {
    fn from(e: std::net::AddrParseError) -> Self {
        Self::AddrParseError(e)
    }
}
impl From<http::Error> for Error {
    fn from(e: http::Error) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<http::uri::InvalidUri> for Error {
    fn from(e: http::uri::InvalidUri) -> Self {
        Self::InvalidUri(e)
    }
}
impl<T: Send + Sync + 'static> From<std::sync::PoisonError<T>> for Error {
    fn from(e: std::sync::PoisonError<T>) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<glob::PatternError> for Error {
    fn from(e: glob::PatternError) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<std::num::ParseIntError> for Error {
    fn from(e: std::num::ParseIntError) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<ThreadPoolBuildError> for Error {
    fn from(e: ThreadPoolBuildError) -> Self {
        Self::Any(Box::new(e))
    }
}
impl From<hyper::Error> for Error {
    fn from(e: hyper::Error) -> Self {
        Self::Any(Box::new(e))
    }
}
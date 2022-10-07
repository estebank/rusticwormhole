use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::error::Error;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::Write;
use std::net::SocketAddr;
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tokio::fs::create_dir_all;
use warp::{Buf, Filter};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

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

#[derive(Debug, Subcommand)]
enum Flavor {
    /// Send a local file to a registered receiver
    Send {
        /// Your username which will be displayed to the receiver
        username: String,
        /// Username of the receiver
        target: String,
        /// File to be sent
        path: PathBuf,
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

#[tokio::main]
async fn main() -> Result<()> {
    let opts: Opts = Opts::parse();
    match opts.flavor {
        Flavor::Send {
            username,
            target,
            path,
        } => send(&username, &target, path, &opts.registry, opts.buf_size).await?,
        Flavor::List => list(&opts.registry).await?,
        Flavor::Receive {
            username,
            port,
            target_dir,
        } => receive(&username, port, target_dir, &opts.registry, opts.buf_size).await?,
        Flavor::Registry => registry(&opts.registry).await.unwrap(),
    }
    Ok(())
}

/// List all the registered receivers.
async fn list(registry: &str) -> Result<()> {
    let client = hyper::Client::new();
    let res = client.get(format!("http://{}", registry).parse()?).await?;
    let body = hyper::body::aggregate(res).await?;
    let map: Map = serde_json::from_reader(body.reader())?;
    println!("Currently registered users:\n");
    for username in map.0.keys() {
        println!("{username}");
    }
    Ok(())
}

/// Send a local file to a registered receiver.
async fn send(
    username: &str,
    target: &str,
    path: PathBuf,
    registry: &str,
    buf_size: usize,
) -> Result<()> {
    if !path.exists() {
        panic!("`{}` doesn't exist", path.display());
    }
    if path.is_dir() {
        panic!("only single files can be sent");
    }
    let client = hyper::Client::new();
    let uri: http::Uri = format!("http://{}", registry).parse()?;

    let filename = path.components().last().unwrap();
    let filename: &std::path::Path = filename.as_ref();

    let res = client.get(uri).await?;
    let body = hyper::body::aggregate(res).await?;
    let map: Map = serde_json::from_reader(body.reader())?;
    let mut file = File::open(&path)?;

    let mut stream = TcpStream::connect(&map.0[target])?;
    let end = file.seek(std::io::SeekFrom::End(0))?;
    let header = format!("{}:{}:{}", username, end, filename.display());
    stream.write_all(header.as_bytes())?;

    let _ = file.seek(std::io::SeekFrom::Start(0))?;
    let mut go_ahead = vec![0];
    let _bytes_read = stream.read(&mut go_ahead)?;
    if go_ahead[0] != 1 {
        panic!("rejected");
    }

    let mut contents = vec![0; buf_size];
    let mut total = 0;
    loop {
        let n = if buf_size == 0 {
            file.read_to_end(&mut contents)?
        } else {
            file.read(&mut contents)?
        };
        stream.write_all(&contents[0..n])?;
        stream.flush()?;
        // print!(".");
        total += n;
        if n == 0 {
            break;
        }
    }
    println!("total {}", total);
    Ok(())
}

/// Set up a receiver service. It can only handle one file at a time because we don't negotiate a
/// new port for each new incomming connection.
async fn receive(
    username: &str,
    port: usize,
    target_dir: PathBuf,
    registry: &str,
    buf_size: usize,
) -> Result<()> {
    println!("Registering username {username} at {registry} to receive files on {port}");
    let client = hyper::Client::new();
    let uri: http::Uri = format!("http://{registry}/register").parse()?;
    let post = hyper::Request::post(uri).body(hyper::Body::from(format!(
        r#"{{ "username": "{username}", "port": {port} }}"#
    )))?;
    println!("{post:?}");
    let _res = client.request(post).await?;
    if !_res.status().is_success() {
        let mut reader = hyper::body::aggregate(_res.into_body()).await?.reader();
        let mut body = String::new();
        reader.read_to_string(&mut body)?;

        #[derive(Debug)]
        struct E;
        impl std::fmt::Display for E {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                write!(f, "error")
            }
        }
        impl std::error::Error for E {}
        return Err(Box::new(E));
    }
    println!("{_res:?}");

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port))?;
    let (stream, _socket_addr) = listener.accept()?;
    create_dir_all(&target_dir).await?;
    println!("created dir {target_dir:?}");

    // let target_dir = target_dir.clone();
    process(stream, target_dir, buf_size).await?;
    // tokio::task::spawn(async move { process(stream, target_dir).await });
    Ok(())
}

unsafe impl Send for ProcessErr {}
unsafe impl Sync for ProcessErr {}

#[derive(Debug)]
enum ProcessErr {
    Io(std::io::Error),
    Utf8(std::str::Utf8Error),
}
impl std::fmt::Display for ProcessErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "{}", err),
            Self::Utf8(err) => write!(f, "{}", err),
        }
    }
}
impl serde::ser::StdError for ProcessErr {}
impl From<std::io::Error> for ProcessErr {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}
impl From<std::str::Utf8Error> for ProcessErr {
    fn from(e: std::str::Utf8Error) -> Self {
        Self::Utf8(e)
    }
}
async fn process(
    mut stream: TcpStream,
    mut path: PathBuf,
    buf_size: usize,
) -> std::result::Result<(), ProcessErr> {
    let mut contents = vec![0; if buf_size == 0 { 1024 } else { buf_size }];
    let n = stream.read(&mut contents)?;
    if n == 0 {
        println!("username and path missing?");
        return Ok(());
    }
    let header = std::str::from_utf8(&contents[..n])?.to_string();
    let header = header.split(':').collect::<Vec<_>>();
    let (username, file_len, file_name) = match &header[..] {
        [username, file_len, file_name] => {
            let file_len: std::result::Result<usize, _> = file_len.parse();
            (username, file_len.unwrap(), file_name)
        }
        _ => panic!(),
    };
    println!("incoming file `{file_name}` from `{username}` with len {file_len}");
    let _bytes_written = stream.write(&[1])?;

    path.push(file_name);

    // If the file already exists we overwrite it.
    println!("writing to {:?}", path.display());
    let mut file = File::create(&path)?;
    let mut total = 0;
    let mut consecutive_zeros = 0;
    loop {
        let n = if buf_size == 0 {
            stream.read_to_end(&mut contents)?
        } else {
            stream.read(&mut contents)?
        };
        std::io::copy(&mut &contents[..n], &mut file)?;
        // print!(".");

        total += n;
        if total == file_len || consecutive_zeros > 1000 {
            break;
        }
        if n == 0 {
            consecutive_zeros += 1;
        }
    }
    println!("total {} of {}", total, file_len);
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

async fn registry(reg: &str) -> std::result::Result<(), std::net::AddrParseError> {
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
                println!("{r}");
                state.lock().unwrap().insert(username, target_addr);
                r
            }
        });
    let routes = register.or(root);
    println!("starting registry listening at `{reg}`");
    let reg: std::net::SocketAddr = reg.parse()?;
    warp::serve(routes).run(reg).await;
    Ok(())
}

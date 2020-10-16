use async_std::fs::{create_dir_all, File};
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use clap::Clap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tide::prelude::*;
use tide::Request;

/// Magic Wormhole clone
#[derive(Clap, Debug)]
#[clap(version = "0.1")]
struct Opts {
    #[clap(subcommand)]
    flavor: Flavor,
}

#[derive(Clap, Debug)]
enum Flavor {
    Send {
        username: String,
        target: String,
        path: PathBuf,
    },
    List,
    Receive {
        username: String,
        port: usize,
    },
    Registry,
}

const BUF_SIZE: usize = 1024;
const REGISTRY: &'static str = "127.0.0.1:9999";

#[async_std::main]
async fn main() -> Result<(), Error> {
    let opts: Opts = Opts::parse();
    let mut users: HashMap<&str, SocketAddr> = HashMap::new();
    users.insert("foo", SocketAddr::from(([127, 0, 0, 1], 8000)));
    users.insert("bar", SocketAddr::from(([127, 0, 0, 1], 8001)));
    match opts.flavor {
        Flavor::Send {
            username,
            target,
            path,
        } => send(&username, &target, path).await?,
        Flavor::List => list().await?,
        Flavor::Receive { username, port } => receive(&username, port).await?,
        Flavor::Registry => registry().await.unwrap(),
    }
    Ok(())
}

#[derive(Debug)]
struct Error;

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Error {
        println!("{:?}", e);
        Error
    }
}

async fn list() -> Result<(), Error> {
    let res = surf::get(format!("http://{}", REGISTRY));
    let map: Map = surf::client().recv_json(res).await.map_err(map_err)?;
    println!("Currently registered users:\n");
    for username in map.0.keys() {
        println!("{}", username);
    }
    Ok(())
}

async fn send(username: &str, target: &str, path: PathBuf) -> Result<(), Error> {
    if !path.exists() {
        panic!("{} doesn't exist", path.display());
    }
    let res = surf::get(format!("http://{}", REGISTRY));

    let map: Map = surf::client().recv_json(res).await.map_err(map_err)?;
    let mut file = File::open(&path).await?;

    let mut stream = TcpStream::connect(&map.0[target]).await?;
    stream.write(username.as_bytes()).await?;
    stream.write(":".as_bytes()).await?;
    stream.write(path.to_string_lossy().as_bytes()).await?;
    let mut go_ahead = vec![0];
    stream.read(&mut go_ahead).await?;
    if go_ahead[0] != 1 {
        panic!("rejected");
    }

    let mut contents = vec![0; BUF_SIZE];
    loop {
        let n = file.read(&mut contents).await?;
        if n == 0 {
            break;
        }
        stream.write(&contents[0..n]).await?;
    }
    Ok(())
}

fn map_err<T: std::fmt::Debug>(e: T) -> Error {
    println!("{:?}", e);
    Error
}

async fn receive(username: &str, port: usize) -> Result<(), Error> {
    // FIXME: Use POST instead of GET
    let _res = surf::get(&format!(
        "http://{}/register?username={}&target={}:{}",
        REGISTRY, username, "127.0.0.1", port
    ))
    .await
    .map_err(map_err)?;

    let listener = TcpListener::bind(format!("127.0.0.1:{}", port)).await?;
    let mut incoming = listener.incoming();
    let target_dir: PathBuf = "received_files".into();
    create_dir_all(&target_dir).await?;

    'outer: while let Some(stream) = incoming.next().await {
        let mut contents = vec![0; BUF_SIZE];
        let mut stream = stream?;

        let n = stream.read(&mut contents).await?;
        if n == 0 {
            println!("username and path missing?");
            break;
        }
        let header = std::str::from_utf8(&contents[..n])?;
        let pos = header.find(':').unwrap();
        let file_name = &header[pos + 1..];
        let username = &header[..pos];
        println!("incoming file `{}` from {}", file_name, username);
        loop {
            println!("accept? y/n");
            let stdin = async_std::io::stdin();
            let mut line = String::new();
            stdin.read_line(&mut line).await?;
            if line.trim().to_ascii_lowercase() == "n" {
                println!("rejecting");
                stream.write(&[0]).await?;
                continue 'outer;
            } else if line.trim().to_ascii_lowercase() == "y" {
                println!("accepting");
                stream.write(&[1]).await?;
                break;
            }
        }

        let mut path = target_dir.clone();
        path.push(file_name);
        // Maintain directory structure
        create_dir_all(&path.parent().unwrap()).await?;

        // TODO file already exists?
        let mut file = File::create(&path).await?;

        println!("writing");
        loop {
            let n = stream.read(&mut contents).await?;
            if n == 0 {
                break;
            }
            let _ = file.write(&contents[..n]).await?;
            print!(".");
        }
        println!("");
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct State {
    registry: Arc<Mutex<HashMap<String, String>>>,
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
struct Map(HashMap<String, String>);

impl State {
    fn register(&self, mapping: Mapping) {
        let r = self.registry.lock();
        let mut r = r.unwrap();
        r.insert(mapping.username.unwrap(), mapping.target.unwrap());
    }
}

unsafe impl Send for State {}
unsafe impl Sync for State {}

#[derive(Deserialize, Default, Debug)]
#[serde(default)]
struct Mapping {
    username: Option<String>,
    target: Option<String>,
}

async fn registry() -> tide::Result<()> {
    let registry = State::default();
    let mut app = tide::with_state(registry);
    app.at("/").get(|req: Request<State>| {
        async move {
            let r = req.state().registry.lock();
            let r = r.unwrap();
            let mut res = tide::Response::new(200);
            let map = Map(r.clone()); // FIXME: don't do this
            res.set_body(tide::Body::from_json(&map)?);
            Ok(res)
        }
    });
    app.at("/register").get(|req: Request<State>| {
        // FIXME: This should be POST not GET
        async move {
            let state = req.state();
            let mapping: Mapping = req.query()?;
            println!("mapping {:?}", mapping);
            state.register(mapping);
            Ok("ok")
        }
    });
    app.listen(REGISTRY).await?;
    Ok(())
}

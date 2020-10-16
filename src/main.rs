use clap::Clap;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use async_std::net::{TcpStream, TcpListener};
use async_std::prelude::*;
use async_std::fs::{File, create_dir_all};
use tide::Request;
use tide::prelude::*;
use std::sync::Arc;
use std::sync::Mutex;


/// Magic Wormhole clone
#[derive(Clap, Debug)]
#[clap(version = "0.1")]
struct Opts {
    #[clap(subcommand)]
    flavor: Flavor,
    username: String,
}

#[derive(Clap, Debug)]
enum Flavor {
    Send(SendTo),
    Receive,
    Registry,
}

#[derive(Clap, Debug)]
struct SendTo {
    name: String,
    path: PathBuf,
}


const BUF_SIZE: usize = 1024;
const REGISTRY: &'static str = "127.0.0.1:9999";

#[async_std::main]
async fn main() -> Result<(), Error> {
    let opts: Opts = Opts::parse();
    let mut users: HashMap<&str, SocketAddr> = HashMap::new();
    users.insert("foo", SocketAddr::from(([127, 0, 0, 1], 8000)));
    users.insert("bar", SocketAddr::from(([127, 0, 0, 1], 8001)));
    println!("{:?}", opts);
    match opts.flavor {
        Flavor::Send(target) => send(&target, &opts.username).await?,
        Flavor::Receive => receive().await?,
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

async fn send(target: &SendTo, username: &str) -> Result<(), Error> {
    if !target.path.exists() {
        panic!("{} doesn't exist", target.path.display());
    }
    let mut file = File::open(&target.path).await?;

    let mut users: HashMap<&str, SocketAddr> = HashMap::new();
    users.insert("foo", SocketAddr::from(([127, 0, 0, 1], 8000)));
    users.insert("bar", SocketAddr::from(([127, 0, 0, 1], 8001)));

    let mut stream = TcpStream::connect(users[target.name.as_str()]).await?;
    stream.write(username.as_bytes()).await?;
    stream.write(":".as_bytes()).await?;
    stream.write(target.path.to_string_lossy().as_bytes()).await?;

    let mut contents = vec![0; BUF_SIZE];
    loop {
        let n = file.read(&mut contents).await?;
        if n == 0 { break; }
        println!("read {:?} {:?}", target.path, contents);
        stream.write(&contents[0..n]).await?;
    }
    Ok(())
}

async fn receive() -> Result<(), Error> {

    let listener = TcpListener::bind("127.0.0.1:8000").await?;
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
        println!("{:?}", header);
        let pos = header.find(':').unwrap();
        let file_name = &header[pos+1..];
        let username = &header[..pos];
        println!("incoming file `{}` from {}", file_name, username);
        loop {
            println!("accept? y/n");
            let stdin = async_std::io::stdin();
            let mut line = String::new();
            stdin.read_line(&mut line).await?;
            if line.trim().to_ascii_lowercase() == "n" {
                println!("rejecting");
                continue 'outer;
            } else if line.trim().to_ascii_lowercase() == "y" {
                println!("accepting");
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
            if n == 0 { break; }
            let _ = file.write(&contents[..n]).await?;
            print!("{}", std::str::from_utf8(&contents[..n])?);
        }
        println!("");
    }
    Ok(())
}

#[derive(Debug, Clone, Default)]
struct State {
    registry: Arc<Mutex<HashMap<String, String>>>,
}

impl State {
    fn register(&self, mapping: Mapping) {
        let r = self.registry.lock();
        let mut r = r.unwrap();
        r.insert(mapping.username.unwrap(), mapping.target.unwrap());
    }
}

unsafe impl Send for State {}
unsafe impl Sync for State {}

#[derive(Deserialize, Default)]
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
            println!("hi");
            let r = req.state().registry.lock();
            let r = r.unwrap();
            let x = r.get(req.url().as_str()).clone();
            Ok(format!("{:?}", x))
        }
    });
    app.at("/").post(|req: Request<State>| {
        async move {
            let state  = req.state();
            let mapping: Mapping = req.query()?;
            state.register(mapping);
            Ok("ok")
        }
    });
    app.listen(REGISTRY).await?;
    Ok(())
}
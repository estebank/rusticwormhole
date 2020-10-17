use async_std::fs::{create_dir_all, File};
use async_std::net::{TcpListener, TcpStream};
use async_std::prelude::*;
use clap::Clap;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use tide::prelude::*;
use tide::Request;

/// Magic Wormhole clone
#[derive(Clap, Debug)]
#[clap(version = "0.1")]
struct Opts {
    #[clap(long, default_value = "0.0.0.0:9999")]
    registry: String,
    #[clap(subcommand)]
    flavor: Flavor,
}

#[derive(Clap, Debug)]
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
        target_dir: Option<PathBuf>,
    },
    /// Start centralized receiver registry
    Registry,
}

/// Size of the send/receive buffers.
const BUF_SIZE: usize = 1024;

#[async_std::main]
async fn main() -> Result<(), Error> {
    let opts: Opts = Opts::parse();
    match opts.flavor {
        Flavor::Send {
            username,
            target,
            path,
        } => send(&username, &target, path, &opts.registry).await?,
        Flavor::List => list(&opts.registry).await?,
        Flavor::Receive {
            username,
            port,
            target_dir,
        } => {
            receive(
                &username,
                port,
                target_dir.unwrap_or("received_files".into()),
                &opts.registry,
            )
            .await?
        }
        Flavor::Registry => registry(&opts.registry).await.unwrap(),
    }
    Ok(())
}

// All of the error handling is hacky at the moment. For a prod tool I would clean this up and add
// extra information to aid the user.

#[derive(Debug)]
struct Error;

impl<E: std::error::Error> From<E> for Error {
    fn from(e: E) -> Error {
        println!("{:?}", e);
        Error
    }
}

// Hack to unify errors
fn map_err<T: std::fmt::Debug>(e: T) -> Error {
    println!("{:?}", e);
    Error
}

/// List all the registered receivers.
async fn list(registry: &str) -> Result<(), Error> {
    let res = surf::get(format!("http://{}", registry));
    let map: Map = surf::client().recv_json(res).await.map_err(map_err)?;
    println!("Currently registered users:\n");
    for username in map.0.keys() {
        println!("{}", username);
    }
    Ok(())
}

/// Send a local file to a registered receiver.
async fn send(username: &str, target: &str, path: PathBuf, registry: &str) -> Result<(), Error> {
    if !path.exists() {
        panic!("{} doesn't exist", path.display());
    }
    let res = surf::get(format!("http://{}", registry));

    let map: Map = surf::client().recv_json(res).await.map_err(map_err)?;
    let mut file = File::open(&path).await?;

    let mut stream = TcpStream::connect(&map.0[target]).await?;
    let end = file.seek(async_std::io::SeekFrom::End(0)).await?;
    let header = format!("{}:{}:{}", username, end, path.to_string_lossy());
    stream.write(header.as_bytes()).await?;

    let _ = file.seek(async_std::io::SeekFrom::Start(0)).await?;
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
        print!(".");
    }
    println!("");
    Ok(())
}

/// Set up a receiver service. It can only handle one file at a time because we don't negotiate a
/// new port for each new incomming connection.
async fn receive(username: &str, port: usize, target_dir: PathBuf, registry: &str) -> Result<(), Error> {
    // FIXME: Use POST instead of GET
    let _res = surf::get(&format!(
        "http://{}/register?username={}&target={}",
        registry, username, port
    ))
    .await
    .map_err(map_err)?;

    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    let mut incoming = listener.incoming();
    create_dir_all(&target_dir).await?;

    'outer: while let Some(stream) = incoming.next().await {
        let mut contents = vec![0; BUF_SIZE];
        let mut stream = stream?;

        let n = stream.read(&mut contents).await?;
        if n == 0 {
            println!("username and path missing?");
            break;
        }
        let header = std::str::from_utf8(&contents[..n])?.to_string();
        let header = header.split(':').collect::<Vec<_>>();
        let (username, file_len, file_name) = match &header[..] {
            [username, file_len, file_name] => (username, file_len, file_name),
            _ => panic!(),
        };
        println!("incoming file `{}` from {} with len {}", file_name, username, file_len);
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

/// Registry DB
#[derive(Debug, Clone, Default)]
struct State {
    registry: Arc<Mutex<HashMap<String, String>>>,
}

/// Used only for JSON creation.
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

async fn registry(reg: &str) -> tide::Result<()> {
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
            let mut mapping: Mapping = req.query()?;
            mapping.target = mapping.target.map(|port| format!("{}:{}", req.remote().unwrap().split(":").next().unwrap(), port));
            println!("mapping {:?}", mapping);
            state.register(mapping);
            Ok("ok")
        }
    });
    app.listen(reg).await?;
    Ok(())
}

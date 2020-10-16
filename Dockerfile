FROM rust:1.47 as builder

RUN USER=root cargo new --bin rusticwormhole 

WORKDIR ./rust-docker-web
COPY ./Cargo.toml Cargo.toml 

ADD ./src/ src/

RUN cargo build --release

ADD ./2g.dat ./2g.dat

CMD ["/bin/bash"]

commands used to build:

docker build -t rusticwormhole .

create test network

docker network create test

run images:

docker run -it --network test --name receiver rusticwormhole:latest bash
docker run -it --network test --name sender rusticwormhole:latest bash
docker run -it --network test --name registry rusticwormhole:latest bash



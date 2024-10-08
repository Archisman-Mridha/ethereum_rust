use bytes::Bytes;
use ethereum_rust_chain::add_block;
use ethereum_rust_core::types::{Block, Genesis};
use ethereum_rust_net::bootnode::BootNode;
use ethereum_rust_net::node_id_from_signing_key;
use ethereum_rust_net::types::Node;
use ethereum_rust_storage::{EngineType, Store};
use k256::{ecdsa::SigningKey, elliptic_curve::rand_core::OsRng};
use std::{
    fs::File,
    io,
    net::{SocketAddr, ToSocketAddrs},
};
use tokio::try_join;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;
mod cli;
mod decode;

#[tokio::main]
async fn main() {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::DEBUG)
        .finish();

    tracing::subscriber::set_global_default(subscriber).expect("setting default subscriber failed");

    let matches = cli::cli().get_matches();

    let http_addr = matches
        .get_one::<String>("http.addr")
        .expect("http.addr is required");
    let http_port = matches
        .get_one::<String>("http.port")
        .expect("http.port is required");
    let authrpc_addr = matches
        .get_one::<String>("authrpc.addr")
        .expect("authrpc.addr is required");
    let authrpc_port = matches
        .get_one::<String>("authrpc.port")
        .expect("authrpc.port is required");
    let authrpc_jwtsecret = matches
        .get_one::<String>("authrpc.jwtsecret")
        .expect("authrpc.jwtsecret is required");

    let tcp_addr = matches
        .get_one::<String>("p2p.addr")
        .expect("addr is required");
    let tcp_port = matches
        .get_one::<String>("p2p.port")
        .expect("port is required");
    let udp_addr = matches
        .get_one::<String>("discovery.addr")
        .expect("discovery.addr is required");
    let udp_port = matches
        .get_one::<String>("discovery.port")
        .expect("discovery.port is required");

    let genesis_file_path = matches
        .get_one::<String>("network")
        .expect("network is required");

    let bootnodes: Vec<BootNode> = matches
        .get_many("bootnodes")
        .map(Iterator::copied)
        .map(Iterator::collect)
        .unwrap_or_default();

    if bootnodes.is_empty() {
        warn!("No bootnodes specified. This node will not be able to connect to the network.");
    }

    let http_socket_addr =
        parse_socket_addr(http_addr, http_port).expect("Failed to parse http address and port");
    let authrpc_socket_addr = parse_socket_addr(authrpc_addr, authrpc_port)
        .expect("Failed to parse authrpc address and port");

    let udp_socket_addr =
        parse_socket_addr(udp_addr, udp_port).expect("Failed to parse discovery address and port");
    let tcp_socket_addr =
        parse_socket_addr(tcp_addr, tcp_port).expect("Failed to parse addr and port");

    let mut store = match matches.get_one::<String>("datadir") {
        Some(data_dir) if !data_dir.is_empty() => Store::new(data_dir, EngineType::Libmdbx),
        _ => Store::new("storage.db", EngineType::InMemory),
    }
    .expect("Failed to create Store");

    let genesis = read_genesis_file(genesis_file_path);
    store
        .add_initial_state(genesis.clone())
        .expect("Failed to create genesis block");

    if let Some(chain_rlp_path) = matches.get_one::<String>("import") {
        let blocks = read_chain_file(chain_rlp_path);
        let size = blocks.len();
        for block in blocks {
            let _ = add_block(&block, &store);
        }
        info!("Added {} blocks to blockchain", size);
    }
    let jwt_secret = read_jwtsecret_file(authrpc_jwtsecret);

    let signer = SigningKey::random(&mut OsRng);
    let local_node_id = node_id_from_signing_key(&signer);

    let local_p2p_node = Node {
        ip: udp_socket_addr.ip(),
        udp_port: udp_socket_addr.port(),
        tcp_port: tcp_socket_addr.port(),
        node_id: local_node_id,
    };

    let rpc_api = ethereum_rust_rpc::start_api(
        http_socket_addr,
        authrpc_socket_addr,
        store,
        jwt_secret,
        local_p2p_node,
    );
    let networking =
        ethereum_rust_net::start_network(udp_socket_addr, tcp_socket_addr, bootnodes, signer);

    try_join!(tokio::spawn(rpc_api), tokio::spawn(networking)).unwrap();
}

fn read_jwtsecret_file(jwt_secret_path: &str) -> Bytes {
    match File::open(jwt_secret_path) {
        Ok(mut file) => decode::jwtsecret_file(&mut file),
        Err(_) => write_jwtsecret_file(jwt_secret_path),
    }
}

fn write_jwtsecret_file(jwt_secret_path: &str) -> Bytes {
    info!("JWT secret not found in the provided path, generating JWT secret");
    let secret = generate_jwt_secret();
    std::fs::write(jwt_secret_path, &secret).expect("Unable to write JWT secret file");
    hex::decode(secret).unwrap().into()
}

fn generate_jwt_secret() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let mut secret = [0u8; 32];
    rng.fill(&mut secret);
    hex::encode(secret)
}

fn read_chain_file(chain_rlp_path: &str) -> Vec<Block> {
    let chain_file = std::fs::File::open(chain_rlp_path).expect("Failed to open chain rlp file");
    decode::chain_file(chain_file).expect("Failed to decode chain rlp file")
}

fn read_genesis_file(genesis_file_path: &str) -> Genesis {
    let genesis_file = std::fs::File::open(genesis_file_path).expect("Failed to open genesis file");
    decode::genesis_file(genesis_file).expect("Failed to decode genesis file")
}

fn parse_socket_addr(addr: &str, port: &str) -> io::Result<SocketAddr> {
    // NOTE: this blocks until hostname can be resolved
    format!("{addr}:{port}")
        .to_socket_addrs()?
        .next()
        .ok_or(io::Error::new(
            io::ErrorKind::NotFound,
            "Failed to parse socket address",
        ))
}

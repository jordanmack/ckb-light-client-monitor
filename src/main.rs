use chrono::{Local, DateTime};
use env_logger::{Builder, Env};
use num_format::{ToFormattedString};
use reqwest;
use serde_json::json;
use std::{env, error::Error, thread, time};
use std::io::Write;

const HOST: &str = "http://127.0.0.1";
const STARTING_PORT: u16 = 19000;
const TOTAL_CLIENTS: usize = 100;
const CHECK_INTERVAL: u64 = 60;
const MAX_BLOCK_DIFF: u64 = 30;

/// Represents a CKB light client.
struct Client 
{
	number: usize,
	port: u16,
	is_online: bool,
	block_number: u64,
	peers: u16,
	time_offline: Option<DateTime<Local>>,
}

impl Client 
{
	/// Creates a new `Client`.
	fn new(number: usize) -> Self 
	{
		Self 
		{
			number,
			port: STARTING_PORT + number as u16,
			is_online: true,
			block_number: 0,
			peers: 0,
			time_offline: None,
		}
	}

	/// Checks if the RPC server of the client is running using the `local_node_info` RPC call.
	async fn check_rpc(&mut self) -> Result<(), Box<dyn Error>>
	{
		let rpc_payload = json!(
		{
			"id": 1,
			"jsonrpc": "2.0",
			"method": "local_node_info",
			"params": []
		});

		let client = reqwest::Client::new();
		let response_result = client.post(format!("{}:{}/", HOST, self.port))
			.json(&rpc_payload)
			.send().await;

		match response_result
		{
			Ok(res) =>
			{
				if res.status().is_success()
				{
					if !self.is_online
					{
						let duration_offline = Local::now().signed_duration_since(self.time_offline.unwrap()).num_seconds();
						log::info!("Client {} is now online. (Offline {} seconds.)", self.number, duration_offline.to_formatted_string(&num_format::Locale::en));

						self.is_online = true;
						self.time_offline = None;
					}
				}
				else
				{
					if self.is_online
					{
						log::error!("Client {} gave an error response.", self.number);
						self.is_online = false;
						self.time_offline = Some(Local::now());
						self.peers = 0;
						self.block_number = 0;
					}
				}
			}
			Err(e) =>
			{
				if self.is_online
				{
					// Handle the specific case where the client does not respond.
					log::error!("Client {} did not respond: {}", self.number, e);
					self.is_online = false;
					self.time_offline = Some(Local::now());
					self.peers = 0;
					self.block_number = 0;
				}
			}
		}

		Ok(())
	}

	/// Checks the number of peers the client is connected to using the `get_peers` RPC call.
	async fn check_peers(&mut self) -> Result<(), Box<dyn Error>>
	{
		if !self.is_online
		{
			return Ok(());
		}

		let rpc_payload = json!(
		{
			"id": 1,
			"jsonrpc": "2.0",
			"method": "get_peers",
			"params": []
		});

		let client = reqwest::Client::new();
		let response_result = client.post(format!("{}:{}/", HOST, self.port))
			.json(&rpc_payload)
			.send().await;

		match response_result
		{
			Ok(res) =>
			{
				let json_result: Result<serde_json::Value, _> = res.json().await;
				match json_result
				{
					Ok(json) =>
					{
						let peers_option = json["result"].as_array();

						match peers_option
						{
							Some(peers) =>
							{
								let peers_count = peers.len();

								// Print a warning if the client peer cound has changed and has 0 or 1 peers.
								if self.peers != peers_count as u16 && (peers_count == 0 || peers_count == 1)
								{
									let plural = if peers_count == 1 { "" } else { "s" };
									log::debug!("Client {} has {} peer{}.", self.number, peers_count, plural);
								}
								self.peers = peers_count as u16;
							},
							None =>
							{
								log::error!("Client {} failed to parse JSON response: 'result' field is not an array or missing", self.number);
							}
						}
					},
					Err(e) =>
					{
						log::error!("Client {} failed to parse JSON response: {}", self.number, e);
					}
				}
			},
			Err(_) =>
			{
				log::error!("Client {} did not respond to the peer request.", self.number);
			}
		}

		Ok(())
	}

	/// Retrieves and updates the current block number of the client using the `get_tip_header` RPC call.
	async fn check_block_number(&mut self) -> Result<(), Box<dyn Error>>
	{
		if !self.is_online
		{
			return Ok(());
		}

		let rpc_payload = json!(
		{
			"id": 1,
			"jsonrpc": "2.0",
			"method": "get_tip_header",
			"params": []
		});

		let client = reqwest::Client::new();
		let response_result = client.post(format!("{}:{}/", HOST, self.port))
			.json(&rpc_payload)
			.send().await;

		if let Err(_) = response_result
		{
			log::error!("Client {} did not respond to the tip request.", self.number);
			return Ok(());
		}

		let response = response_result.unwrap();
		let json_result = response.json::<serde_json::Value>().await;

		if let Err(e) = json_result
		{
			log::error!("Client {} failed to parse JSON response: {}", self.number, e);
			return Ok(());
		}

		let json = json_result.unwrap();
		if let Some(header) = json["result"].get("number")
		{
			match header.as_str()
			{
				Some(block_num_str) =>
				{
					let block_num = u64::from_str_radix(block_num_str.trim_start_matches("0x"), 16);
					
					match block_num
					{
						Ok(num) => { self.block_number = num; },
						Err(e) =>
						{
							log::error!("Client {} failed to parse block number: {}", self.number, e);
						}
					};
				},
				None =>
				{
					log::error!("Client {} returned a block number in an unexpected format.", self.number);
				}
			}
		}
		else
		{
			log::error!("Client {} returned an unexpected JSON object.", self.number);
		}

		Ok(())
	}
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> 
{
	// Initialize the logger with a default log level.
	let is_verbose = env::args().any(|arg| arg == "-v");
	let logger_level = if is_verbose { "debug" } else { "info" };
	Builder::from_env(Env::default().default_filter_or(logger_level))
		.format(|buf, rec| writeln!(buf, "{} [{}] {}", Local::now().format("%Y%m%d %H:%M:%S"), rec.level(), rec.args()))
		.init();

	let mut clients = (0..TOTAL_CLIENTS).map(Client::new).collect::<Vec<_>>();
	let mut highest_block_number = 0;

	loop
	{
		// Check all clients online status, peer count, and tip block number.
		for client in clients.iter_mut() 
		{
			log::debug!("Checking client {}.", client.number);

			client.check_rpc().await?;
			if client.is_online
			{
				client.check_peers().await?;
				client.check_block_number().await?;

				if client.block_number > highest_block_number 
				{
					highest_block_number = client.block_number;
				}
			}
		}

		// Print warnings for all lagging clients.
		for client in clients.iter()
		{
			if client.is_online && highest_block_number > client.block_number + MAX_BLOCK_DIFF 
			{
				let block_difference = (highest_block_number - client.block_number).to_formatted_string(&num_format::Locale::en);
				let client_block_tip = client.block_number.to_formatted_string(&num_format::Locale::en);
				log::warn!("Client {} is lagging by {} blocks: {}", client.number, block_difference, client_block_tip);
			}
		}

		// Count offline clients from a collection and print a warning if any are found.
		let mut peer_0_clients = Vec::new();
		let mut peer_1_clients = Vec::new();
		let mut offline_clients = Vec::new();
		for client in clients.iter()
		{
			if client.is_online
			{
				if client.peers == 0
				{
					peer_0_clients.push(client.number);
				}
				else if client.peers == 1
				{
					peer_1_clients.push(client.number);
				}
			}
			else
			{
				offline_clients.push(client.number);
			}
		}
		if !peer_0_clients.is_empty()
		{
			let peer_0_client_count = peer_0_clients.len();
			let peer_0_client_string: String = peer_0_clients.iter().map(|x|x.to_string()).collect::<Vec<String>>().join(", ");
			log::info!("There are {} clients with 0 peers: {}", peer_0_client_count, peer_0_client_string);
		}
		if !peer_1_clients.is_empty()
		{
			let peer_1_client_count = peer_1_clients.len();
			let peer_1_client_string = peer_1_clients.iter().map(|x|x.to_string()).collect::<Vec<String>>().join(", ");
			log::info!("There are {} clients with 1 peer: {}", peer_1_client_count, peer_1_client_string);
		}
		if !offline_clients.is_empty()
		{
			let offline_client_count = offline_clients.len();
			let offline_client_string = offline_clients.iter().map(|x|x.to_string()).collect::<Vec<String>>().join(", ");
			log::info!("There are {} clients that are offline: {}", offline_client_count, offline_client_string);
		}

		thread::sleep(time::Duration::from_secs(CHECK_INTERVAL));
	}
}

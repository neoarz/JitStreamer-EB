// Jackson Coxson

use axum::{body::Bytes, http::StatusCode};
use log::info;
use plist::Dictionary;
use sha2::Digest;
use sqlite::State;

/// Check to make sure the Wireguard interface exists
pub fn check_wireguard() {
    let key = wg_config::WgKey::generate_private_key().expect("failed to generate key");
    let interface = wg_config::WgInterface::new(
        key,
        "fd00::/128".parse().unwrap(),
        Some(51869),
        None,
        None,
        None,
    )
    .unwrap();

    wg_config::WgConf::create("/etc/wireguard/jitstreamer.conf", interface, None)
        .expect("failed to create config");

    info!("Created new Wireguard config");
}

/// Takes the plist in bytes, and returns either the pairing file in return or an error message
pub async fn register(plist_bytes: Bytes) -> Result<Bytes, (StatusCode, &'static str)> {
    let plist = match plist::from_bytes::<Dictionary>(plist_bytes.as_ref()) {
        Ok(plist) => plist,
        Err(_) => return Err((StatusCode::BAD_REQUEST, "bad plist")),
    };
    let udid = match plist.get("UDID") {
        Some(plist::Value::String(udid)) => udid,
        _ => return Err((StatusCode::BAD_REQUEST, "no UDID")),
    }
    .to_owned();

    let cloned_udid = udid.clone();
    // Reverse lookup the device to see if we already have an IP for it
    let ip = match tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                info!("Failed to open database: {:?}", e);
                return None;
            }
        };

        // Get the device from the database
        let query = "SELECT ip FROM devices WHERE udid = ?";
        let mut statement = db.prepare(query).unwrap();
        statement
            .bind((1, cloned_udid.to_string().as_str()))
            .unwrap();
        if let Ok(State::Row) = statement.next() {
            let ip = statement.read::<String, _>("ip").unwrap();
            info!("Found device with udid {} already in db", cloned_udid);

            // Delete the device from the database
            let query = "DELETE FROM devices WHERE udid = ?";
            let mut statement = db.prepare(query).unwrap();
            statement
                .bind((1, cloned_udid.to_string().as_str()))
                .unwrap();
            statement.next().unwrap();

            Some(ip)
        } else {
            None
        }
    })
    .await
    {
        Ok(ip) => ip,
        Err(e) => {
            info!("Failed to get IP from database: {:?}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to get IP"));
        }
    };

    // Read the Wireguard config file
    let mut server_peer = match wg_config::WgConf::open("/etc/wireguard/jitstreamer.conf") {
        Ok(conf) => conf,
        Err(e) => {
            info!("Failed to open Wireguard config: {:?}", e);
            if let wg_config::WgConfError::NotFound(_) = e {
                // Generate a new one

                let key = wg_config::WgKey::generate_private_key().expect("failed to generate key");
                let interface = wg_config::WgInterface::new(
                    key,
                    "fd00::/128".parse().unwrap(),
                    Some(51869),
                    None,
                    None,
                    None,
                )
                .unwrap();

                wg_config::WgConf::create("/etc/wireguard/jitstreamer.conf", interface, None)
                    .expect("failed to create config");

                info!("Created new Wireguard config");

                wg_config::WgConf::open("/etc/wireguard/jitstreamer.conf").unwrap()
            } else {
                return Err((
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "failed to open server Wireguard config",
                ));
            }
        }
    };

    let mut public_ip = None;
    if let Some(ip) = ip {
        match server_peer.peers() {
            Ok(peers) => {
                for peer in peers {
                    let peer_ip = peer.allowed_ips();
                    if ip.is_empty() {
                        continue;
                    }
                    if peer_ip[0].to_string() == ip {
                        info!("Found peer with IP {}", ip);

                        public_ip = Some(peer.public_key().to_owned());
                    }
                }
            }
            Err(e) => {
                info!("Failed to get peers: {:?}", e);
                return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to get peers"));
            }
        }
    }

    if let Some(public_ip) = public_ip {
        server_peer = server_peer.remove_peer_by_pub_key(&public_ip).unwrap();
    }

    let ip = generate_ipv6_from_udid(udid.as_str());

    // Generate a new peer for the device
    let client_config = match server_peer.generate_peer(
        std::net::IpAddr::V6(ip),
        "jitstreamer.jkcoxson.com".parse().unwrap(),
        vec!["fd00::/64".parse().unwrap()],
        None,
        true,
        Some(20),
    ) {
        Ok(config) => config.to_string().as_bytes().to_vec(),
        Err(e) => {
            info!("Failed to generate peer: {:?}", e);
            return Err((StatusCode::INTERNAL_SERVER_ERROR, "failed to generate peer"));
        }
    };

    // Save the plist to the storage
    tokio::fs::write(
        format!("/var/lib/lockdown/{udid}.plist"),
        &plist_bytes.to_vec(),
    )
    .await
    .map_err(|e| {
        info!("Failed to save plist: {:?}", e);
        (StatusCode::INTERNAL_SERVER_ERROR, "failed to save plist")
    })?;

    // Save the IP to the database
    tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                info!("Failed to open database: {:?}", e);
                return;
            }
        };

        // Insert the device into the database
        let query = "INSERT INTO devices (udid, ip, last_used) VALUES (?, ?, CURRENT_TIMESTAMP)";
        let mut statement = db.prepare(query).unwrap();
        statement
            .bind(&[(1, udid.as_str()), (2, ip.to_string().as_str())][..])
            .unwrap();
        statement.next().unwrap();
    });

    refresh_wireguard();

    Ok(client_config.into())
}

fn generate_ipv6_from_udid(udid: &str) -> std::net::Ipv6Addr {
    // Hash the UDID using SHA-256
    let mut hasher = sha2::Sha256::new();
    hasher.update(udid.as_bytes());
    let hash = hasher.finalize();

    // Use the first 64 bits of the hash for the interface ID
    let interface_id = u64::from_be_bytes(hash[0..8].try_into().unwrap());

    // Set the first 64 bits to the `fd00::/8` range (locally assigned address)
    let mut segments = [0u16; 8];
    segments[0] = 0xfd00; // First segment in the `fd00::/8` range
    (1..8).for_each(|i| {
        let shift = (7 - i) * 16;
        segments[i] = if shift < 64 {
            ((interface_id >> shift) & 0xFFFF) as u16
        } else {
            0
        };
    });

    std::net::Ipv6Addr::from(segments)
}

fn refresh_wireguard() {
    // wg syncconf jitstreamer <(wg-quick strip jitstreamer)
    let output = std::process::Command::new("bash")
        .arg("-c")
        .arg("wg syncconf jitstreamer <(wg-quick strip jitstreamer)")
        .output()
        .expect("failed to execute process");

    info!("Refreshing Wireguard: {:?}", output);
}

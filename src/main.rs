// Jackson Coxson
// JitStreamer for the year of our Lord, 2025

const VERSION: [u8; 3] = [0, 0, 1];

use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use axum::{
    extract::{Json, Path},
    http::{header::CONTENT_TYPE, Method},
    routing::{get, post},
};
use axum_client_ip::SecureClientIp;
use idevice::{
    installation_proxy::InstallationProxyClient, lockdownd::LockdowndClient, mounter::ImageMounter,
    pairing_file::PairingFile, Idevice,
};
use log::info;
use serde::{Deserialize, Serialize};
use sqlite::State;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixStream,
};
use tower_http::cors::CorsLayer;

mod debug_server;
mod mount;
mod raw_packet;
mod register;
mod runner;
mod tunneld;

#[tokio::main]
async fn main() {
    println!("Starting JitStreamer-EB, enabling logger");
    dotenvy::dotenv().ok();

    env_logger::init();
    info!("Logger initialized");

    // Run the environment checks
    register::check_wireguard();
    if !std::fs::exists("jitstreamer.db").unwrap() {
        info!("Creating database");
        let db = sqlite::open("jitstreamer.db").unwrap();
        db.execute(include_str!("sql/up.sql")).unwrap();
    }

    // Read the environment variable constants
    let runner_count = std::env::var("RUNNER_COUNT")
        .unwrap_or("10".to_string())
        .parse::<u32>()
        .unwrap();
    let allow_registration = std::env::var("ALLOW_REGISTRATION")
        .unwrap_or("1".to_string())
        .parse::<u8>()
        .unwrap()
        == 1;
    let port = std::env::var("JITSTREAMER_PORT")
        .unwrap_or("9172".to_string())
        .parse::<u16>()
        .unwrap();

    // Empty the queues
    mount::empty().await;
    debug_server::empty().await;

    // Run the Python shims
    runner::run("src/runners/mount.py", runner_count);
    runner::run("src/runners/launch.py", runner_count);

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_origin(tower_http::cors::Any)
        .allow_headers([CONTENT_TYPE]);

    // Endpoints (ideas)
    // / get_apps - Gets the list of apps with get-task-allow on the device
    //  - Get the IP from the request and UDID from the database
    //  - Send the udid/IP to netmuxd for heartbeat-ing
    //  - Connect to the device and get the list of bundle IDs
    // / launch_app/<bundle ID> - Launches an app on the device
    //  - Get the IP from the request and UDID from the database
    //  - Make sure netmuxd still has the device
    //  - Check the mounted images for the developer disk image
    //    - If not mounted, add the device to the queue for mounting
    //    - Return a message letting the user know the device is mounting
    //  - Connect to tunneld and get the interface and port for the developer service
    //  - Send the commands to launch the app and detach
    //  - Set last_used to now in the database
    // / register - Registers a device with the server, trade UDID for Wireguard config
    //  - Get the UDID from the plist
    //  - Generate a Wireguard config for the device
    //  - Add the device to the database
    //  - Create a temporary download link for the Wireguard config, add to database
    //  - Return the temporary link to the Wireguard config
    // / mount_queue - Gets position in the mounting queue
    //  - Get the IP from the request and UDID from the database
    //  - Check the queue for the device
    // / mount - Mounts the developer disk image on the device
    //  - Get the IP from the request and UDID from the database
    //  - Make sure netmuxd still has the device
    //  - Add the device to the queue for mounting
    // / change_pairing - Changes the pairing record for the device
    //  - Get the IP from the request and UDID from the database
    //  - Upload the new pairing record from the POST body
    //  - Add the device to netmuxd to make sure it works
    // / get_config/<code>/JitStreamer.conf - Gets the Wireguard config for the device
    //  - Look up the temporary link in the database

    // Start with Axum
    let app = axum::Router::new()
        .layer(cors.clone())
        .route("/hello", get(|| async { "Hello, world!" }))
        .route("/version", post(version))
        .route("/get_apps", get(get_apps))
        .route("/launch_app/{bundle_id}", get(launch_app));

    let app = if allow_registration {
        app.route("/register", post(register::register))
    } else {
        app
    };

    let app = app
        .layer(axum_client_ip::SecureClientIpSource::ConnectInfo.into_extension())
        .layer(cors);

    let addr = SocketAddr::new(IpAddr::from_str("::0").unwrap(), port);
    info!("Starting server on {:?}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .unwrap();
}

const NETMUXD_SOCKET: &str = "/var/run/usbmuxd";
const SERVICE_NAME: &str = "apple-mobdev2";
const SERVICE_PROTOCOL: &str = "tcp";

#[derive(Serialize, Deserialize)]
struct VersionRequest {
    version: String,
}
#[derive(Serialize, Deserialize)]
struct VersionResponse {
    ok: bool,
}

async fn version(Json(version): Json<VersionRequest>) -> Json<VersionResponse> {
    info!("Checking version {}", version.version);

    // Parse the version as 3 numbers
    let version = version
        .version
        .split('.')
        .map(|v| v.parse::<u8>().unwrap_or(0))
        .collect::<Vec<u8>>();

    // Compare the version, compare each number
    for (i, v) in VERSION.iter().enumerate() {
        if version.get(i).unwrap_or(&0) < v {
            return Json(VersionResponse { ok: false });
        }
    }

    Json(VersionResponse { ok: true })
}

#[derive(Serialize, Deserialize, Clone)]
struct GetAppsReturn {
    ok: bool,
    apps: Vec<String>,
    error: Option<String>,
}

/// Gets the list of apps with get-task-allow on the device
///  - Get the IP from the request and UDID from the database
///  - Send the udid/IP to netmuxd for heartbeat-ing
///  - Connect to the device and get the list of bundle IDs
#[axum::debug_handler]
async fn get_apps(ip: SecureClientIp) -> Json<GetAppsReturn> {
    let ip = ip.0;

    info!("Got request to get apps from {:?}", ip);

    let udid = match tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                info!("Failed to open database: {:?}", e);
                return Err(Json(GetAppsReturn {
                    ok: false,
                    apps: Vec::new(),
                    error: Some("Failed to open database".to_string()),
                }));
            }
        };

        // Get the device from the database
        let query = "SELECT udid FROM devices WHERE ip = ?";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, ip.to_string().as_str())).unwrap();
        let udid = if let Ok(State::Row) = statement.next() {
            let udid = statement.read::<String, _>("udid").unwrap();
            info!("Found device with udid {}", udid);
            udid
        } else {
            info!("No device found for IP {:?}", ip);
            return Err(Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("No device found".to_string()),
            }));
        };
        Ok(udid)
    })
    .await
    .unwrap()
    {
        Ok(udid) => udid,
        Err(e) => {
            return e;
        }
    };

    // Get the pairing file
    let pairing_file = match get_pairing_file(udid.clone()).await {
        Ok(pairing_file) => pairing_file,
        Err(e) => {
            info!("Failed to get pairing file: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to get pairing file".to_string()),
            });
        }
    };

    // Check if there are any launches queued
    match debug_server::get_queue_info(&udid).await {
        debug_server::LaunchQueueInfo::Position(p) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some(format!(
                    "Your device is currently in the queue for launching. You are in position {p}."
                )),
            });
        }
        debug_server::LaunchQueueInfo::NotInQueue => {}
        debug_server::LaunchQueueInfo::Error(e) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some(e),
            });
        }
        debug_server::LaunchQueueInfo::ServerError => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to get launch status".to_string()),
            });
        }
    }

    // Check if there are any mounts queued
    match mount::get_queue_info(&udid).await {
        mount::MountQueueInfo::Position(p) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some(format!(
                    "Your device is currently in the queue for mounting. You are in position {p}."
                )),
            });
        }
        mount::MountQueueInfo::NotInQueue => {}
        mount::MountQueueInfo::Error(e) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some(e),
            });
        }
        mount::MountQueueInfo::ServerError => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to get mounting status".to_string()),
            });
        }
        mount::MountQueueInfo::InProgress => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some(
                    "Your device is currently mounting the developer disk image".to_string(),
                ),
            });
        }
    }

    // Make sure netmuxd is heartbeat-ing the device
    // Send a plist to add the device to netmuxd
    if add_device(ip, &udid).await {
        info!("Device {udid} added to netmuxd");
    } else {
        info!("Failed to add device to netmuxd");
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            error: Some("Failed to add device to netmuxd".to_string()),
        });
    }

    // Wait for tunneld to connect
    if !tunneld::wait_for_connection(&udid, 10).await {
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            error: Some("Tunneld failed to connect to the device within 10 seconds".to_string()),
        });
    }

    // Connect to the device and get the list of bundle IDs
    let socket = SocketAddr::new(ip, idevice::lockdownd::LOCKDOWND_PORT);

    let socket = tokio::net::TcpStream::connect(socket).await.unwrap();
    let socket = Box::new(socket);
    let idevice = Idevice::new(socket, "JitStreamer");

    let mut lockdown_client = LockdowndClient { idevice };
    if let Err(e) = lockdown_client.start_session(&pairing_file).await {
        info!("Failed to start session: {:?}", e);
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            error: Some("Failed to start session".to_string()),
        });
    }

    let port = match lockdown_client
        .start_service("com.apple.mobile.installation_proxy")
        .await
    {
        Ok(port) => port.0,
        Err(e) => {
            info!("Failed to start service: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to start service".to_string()),
            });
        }
    };

    let socket = SocketAddr::new(ip, port);
    let socket = match tokio::net::TcpStream::connect(socket).await {
        Ok(socket) => socket,
        Err(e) => {
            info!("Failed to connect to installation_proxy: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to connect to installation_proxy".to_string()),
            });
        }
    };
    let socket = Box::new(socket);
    let mut idevice = Idevice::new(socket, "JitStreamer");

    if let Err(e) = idevice.start_session(&pairing_file).await {
        info!("Failed to start session: {:?}", e);
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            error: Some("Failed to start session".to_string()),
        });
    }

    let mut instproxy_client = InstallationProxyClient::new(idevice);
    let apps = match instproxy_client.get_apps(None, None).await {
        Ok(apps) => apps,
        Err(e) => {
            info!("Failed to get apps: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                error: Some("Failed to get apps".to_string()),
            });
        }
    };
    let apps: Vec<String> = apps
        .into_iter()
        .filter(|(_, app)| {
            // Filter out apps that don't have get-task-allow
            let app = match app {
                plist::Value::Dictionary(app) => app,
                _ => return false,
            };

            match app.get("Entitlements") {
                Some(plist::Value::Dictionary(entitlements)) => {
                    matches!(
                        entitlements.get("get-task-allow"),
                        Some(plist::Value::Boolean(true))
                    )
                }
                _ => false,
            }
        })
        .map(|(bundle_id, _)| bundle_id)
        .collect();

    if apps.is_empty() {
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            error: Some("No apps with get-task-allow found".to_string()),
        });
    }

    Json(GetAppsReturn {
        ok: true,
        apps,
        error: None,
    })
}

#[derive(Serialize, Deserialize)]
struct LaunchAppReturn {
    ok: bool,
    position: Option<usize>,
    error: Option<String>,
}
///  - Get the IP from the request and UDID from the database
/// - Make sure netmuxd still has the device
///  - Check the mounted images for the developer disk image
///    - If not mounted, add the device to the queue for mounting
///    - Return a message letting the user know the device is mounting
///  - Connect to tunneld and get the interface and port for the developer service
///  - Send the commands to launch the app and detach
///  - Set last_used to now in the database
async fn launch_app(ip: SecureClientIp, Path(bundle_id): Path<String>) -> Json<LaunchAppReturn> {
    let ip = ip.0;

    info!("Got request to launch {bundle_id} from {:?}", ip);

    let udid = match tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                info!("Failed to open database: {:?}", e);
                return Err(Json(LaunchAppReturn {
                    ok: false,
                    position: None,
                    error: Some("Failed to open database".to_string()),
                }));
            }
        };

        // Get the device from the database
        let query = "SELECT udid FROM devices WHERE ip = ?";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, ip.to_string().as_str())).unwrap();
        let udid = if let Ok(State::Row) = statement.next() {
            let udid = statement.read::<String, _>("udid").unwrap();
            info!("Found device with udid {}", udid);
            udid
        } else {
            info!("No device found for IP {:?}", ip);
            return Err(Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("No device found".to_string()),
            }));
        };
        Ok(udid)
    })
    .await
    .unwrap()
    {
        Ok(udid) => udid,
        Err(e) => {
            return e;
        }
    };

    // Check the mounting status
    match mount::get_queue_info(&udid).await {
        mount::MountQueueInfo::Position(p) => {
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some(format!(
                    "Your device is currently in the queue for mounting. You are in position {p}."
                )),
            });
        }
        mount::MountQueueInfo::NotInQueue => {}
        mount::MountQueueInfo::Error(e) => {
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some(e),
            });
        }
        mount::MountQueueInfo::ServerError => {
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to get mounting status".to_string()),
            });
        }
        mount::MountQueueInfo::InProgress => {
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some(
                    "Your device is currently mounting the developer disk image".to_string(),
                ),
            });
        }
    }

    // Get the pairing file
    let pairing_file = match get_pairing_file(udid.clone()).await {
        Ok(pairing_file) => pairing_file,
        Err(e) => {
            info!("Failed to get pairing file: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to get pairing file".to_string()),
            });
        }
    };

    // Make sure netmuxd is heartbeat-ing the device
    // Send a plist to add the device to netmuxd
    if add_device(ip, &udid).await {
        info!("Device {udid} added to netmuxd");
    } else {
        info!("Failed to add device to netmuxd");
        return Json(LaunchAppReturn {
            ok: false,
            position: None,
            error: Some("Failed to add device to netmuxd".to_string()),
        });
    }

    // Wait for tunneld to connect
    if !tunneld::wait_for_connection(&udid, 10).await {
        return Json(LaunchAppReturn {
            ok: false,
            position: None,
            error: Some("Tunneld failed to connect to the device within 10 seconds".to_string()),
        });
    }

    // Get the list of mounted images
    let socket = SocketAddr::new(ip, idevice::lockdownd::LOCKDOWND_PORT);
    let socket = match tokio::net::TcpStream::connect(socket).await {
        Ok(socket) => socket,
        Err(e) => {
            info!("Failed to connect to lockdownd: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to connect to lockdownd".to_string()),
            });
        }
    };

    let socket = Box::new(socket);
    let idevice = Idevice::new(socket, "JitStreamer");

    let mut lockdown_client = LockdowndClient { idevice };
    if let Err(e) = lockdown_client.start_session(&pairing_file).await {
        info!("Failed to start session: {:?}", e);
        return Json(LaunchAppReturn {
            ok: false,
            position: None,
            error: Some("Failed to start session".to_string()),
        });
    }

    let port = match lockdown_client
        .start_service("com.apple.mobile.mobile_image_mounter")
        .await
    {
        Ok(port) => port.0,
        Err(e) => {
            info!("Failed to start service: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to start service".to_string()),
            });
        }
    };

    let socket = SocketAddr::new(ip, port);
    let socket = match tokio::net::TcpStream::connect(socket).await {
        Ok(socket) => socket,
        Err(e) => {
            info!("Failed to connect to image_mounter: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to connect to image_mounter".to_string()),
            });
        }
    };
    let socket = Box::new(socket);
    let mut idevice = Idevice::new(socket, "JitStreamer");

    if let Err(e) = idevice.start_session(&pairing_file).await {
        info!("Failed to start session: {:?}", e);
        return Json(LaunchAppReturn {
            ok: false,
            position: None,
            error: Some("Failed to start session".to_string()),
        });
    }

    let mut mounter_client = ImageMounter::new(idevice);
    let images = match mounter_client.copy_devices().await {
        Ok(images) => images,
        Err(e) => {
            info!("Failed to get images: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                position: None,
                error: Some("Failed to get images".to_string()),
            });
        }
    };

    let mut mounted = false;
    for image in images {
        let mut buf = Vec::new();
        let mut writer = std::io::Cursor::new(&mut buf);
        plist::to_writer_xml(&mut writer, &image).unwrap();

        let image = String::from_utf8_lossy(&buf);
        if image.contains("Developer") {
            mounted = true;
            break;
        }
    }

    if !mounted {
        // Add the device to the queue for mounting
        mount::add_to_queue(&udid).await;

        // Return a message letting the user know the device is mounting
        return Json(LaunchAppReturn {
            ok: false,
                position: None,
            error: Some("Your device has been added to the queue for image mounting. Please try again in a minute.".to_string()),
        });
    }

    // Add the launch to the queue
    match debug_server::add_to_queue(&udid, &bundle_id).await {
        Some(position) => Json(LaunchAppReturn {
            ok: true,
            position: Some(position as usize),
            error: None,
        }),
        None => Json(LaunchAppReturn {
            ok: false,
            position: None,
            error: Some("Failed to add to queue".to_string()),
        }),
    }
}

/// Connects to the unix socket and adds the device
async fn add_device(ip: IpAddr, udid: &str) -> bool {
    let mut stream = UnixStream::connect(NETMUXD_SOCKET)
        .await
        .expect("Could not connect to netmuxd socket, is it running?");

    let mut request = plist::Dictionary::new();
    request.insert("MessageType".into(), "AddDevice".into());
    request.insert("ConnectionType".into(), "Network".into());
    request.insert(
        "ServiceName".into(),
        format!("_{}._{}.local", SERVICE_NAME, SERVICE_PROTOCOL).into(),
    );
    request.insert("IPAddress".into(), ip.to_string().into());
    request.insert("DeviceID".into(), udid.into());

    let request = raw_packet::RawPacket::new(request, 69, 69, 69);
    let request: Vec<u8> = request.into();

    stream.write_all(&request).await.unwrap();

    let mut buf = Vec::new();
    let size = stream.read_to_end(&mut buf).await.unwrap();

    let buffer = &mut buf[0..size].to_vec();
    if size == 16 {
        let packet_size = &buffer[0..4];
        let packet_size = u32::from_le_bytes(packet_size.try_into().unwrap());
        // Pull the rest of the packet
        let mut packet = vec![0; packet_size as usize];
        let _ = stream.read(&mut packet).await.unwrap();
        // Append the packet to the buffer
        buffer.append(&mut packet);
    }

    let parsed: raw_packet::RawPacket = buffer.try_into().unwrap();
    match parsed.plist.get("Result") {
        Some(plist::Value::Integer(r)) => r.as_unsigned().unwrap() == 1,
        _ => false,
    }
}

/// Gets the pairing file
async fn get_pairing_file(udid: String) -> Result<PairingFile, idevice::IdeviceError> {
    // All pairing files are stored at /var/lib/lockdown/<udid>.plist
    let path = format!("/var/lib/lockdown/{}.plist", udid);
    let pairing_file = tokio::fs::read(path).await?;

    PairingFile::from_bytes(&pairing_file)
}

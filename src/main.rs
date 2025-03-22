// Jackson Coxson
// JitStreamer for the year of our Lord, 2025

const VERSION: [u8; 3] = [0, 2, 0];

use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    str::FromStr,
};

use axum::{
    extract::{Json, Path, State},
    http::{header::CONTENT_TYPE, Method},
    response::Html,
    routing::{any, get, post},
};
use axum_client_ip::SecureClientIp;
use common::get_pairing_file;
use heartbeat::NewHeartbeatSender;
use idevice::{
    core_device_proxy::CoreDeviceProxy, debug_proxy::DebugProxyClient,
    installation_proxy::InstallationProxyClient, provider::TcpProvider, IdeviceService,
};
use log::{debug, info};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

mod common;
mod db;
mod heartbeat;
mod mount;
mod raw_packet;
mod register;

#[derive(Clone)]
struct JitStreamerState {
    pub new_heartbeat_sender: NewHeartbeatSender,
    pub mount_cache: mount::MountCache,
    pub pairing_file_storage: String,
}

#[tokio::main]
async fn main() {
    println!("Starting JitStreamer-EB, enabling logger");
    dotenvy::dotenv().ok();

    // Read the environment variable constants
    let allow_registration = std::env::var("ALLOW_REGISTRATION")
        .unwrap_or("1".to_string())
        .parse::<u8>()
        .unwrap();
    let port = std::env::var("JITSTREAMER_PORT")
        .unwrap_or("9172".to_string())
        .parse::<u16>()
        .unwrap();
    let pairing_file_storage =
        std::env::var("PLIST_STORAGE").unwrap_or("/var/lib/lockdown".to_string());

    env_logger::init();
    info!("Logger initialized");

    // Run the environment checks
    if allow_registration == 1 {
        register::check_wireguard();
    }
    if !std::fs::exists("jitstreamer.db").unwrap() {
        info!("Creating database");
        let db = sqlite::open("jitstreamer.db").unwrap();
        db.execute(include_str!("sql/up.sql")).unwrap();
    }

    // Create a heartbeat manager
    let state = JitStreamerState {
        new_heartbeat_sender: heartbeat::heartbeat(),
        mount_cache: mount::MountCache::default(),
        pairing_file_storage,
    };

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_origin(tower_http::cors::Any)
        .allow_headers([CONTENT_TYPE]);

    // Start with Axum
    let app = axum::Router::new()
        .layer(cors.clone())
        .route("/hello", get(|| async { "Hello, world!" }))
        .route("/version", post(version))
        .route("/mount", get(mount::check_mount))
        .route("/mount_ws", any(mount::handler))
        .route(
            "/mount_status",
            get(|| async { Html(include_str!("mount.html")) }),
        )
        .route("/get_apps", get(get_apps))
        .route("/launch_app/{bundle_id}", get(launch_app))
        .route("/attach/{pid}", post(attach_app))
        .route("/status", get(status)) // will be removed soon
        .with_state(state);

    let app = if allow_registration == 1 {
        app.route("/register", post(register::register))
    } else if allow_registration == 2 {
        app.route("/register", post(register::register))
            .route("/upload", get(register::upload))
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
    bundle_ids: Option<HashMap<String, String>>,
    error: Option<String>,
}

/// Gets the list of apps with get-task-allow on the device
///  - Get the IP from the request and UDID from the database
///  - Send the udid/IP to netmuxd for heartbeat-ing
///  - Connect to the device and get the list of bundle IDs
#[axum::debug_handler]
async fn get_apps(
    ip: SecureClientIp,
    State(state): State<JitStreamerState>,
) -> Json<GetAppsReturn> {
    let ip = ip.0;

    info!("Got request to get apps from {:?}", ip);

    let udid = match common::get_udid_from_ip(ip.to_string()).await {
        Ok(u) => u,
        Err(e) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                bundle_ids: None,
                error: Some(e),
            })
        }
    };

    // Get the pairing file
    debug!("Getting pairing file for {udid}");
    let pairing_file = match get_pairing_file(&udid, &state.pairing_file_storage).await {
        Ok(pairing_file) => pairing_file,
        Err(e) => {
            info!("Failed to get pairing file: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                bundle_ids: None,
                error: Some(format!("Failed to get pairing file: {:?}", e)),
            });
        }
    };

    // Heartbeat the device
    match heartbeat::heartbeat_thread(udid.clone(), ip, &pairing_file).await {
        Ok(s) => {
            state
                .new_heartbeat_sender
                .send(heartbeat::SendRequest::Store((udid.clone(), s)))
                .await
                .unwrap();
        }
        Err(e) => {
            let e = match e {
                idevice::IdeviceError::InvalidHostID => {
                    "your pairing file is invalid. Regenerate it with jitterbug pair.".to_string()
                }
                _ => e.to_string(),
            };
            info!("Failed to heartbeat device: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                bundle_ids: None,
                error: Some(format!("Failed to heartbeat device: {e}")),
            });
        }
    }

    // Connect to the device and get the list of bundle IDs
    debug!("Connecting to device {udid} to get apps");

    let provider = TcpProvider {
        addr: ip,
        pairing_file,
        label: "JitStreamer-EB".to_string(),
    };

    let mut instproxy_client = match InstallationProxyClient::connect(&provider).await {
        Ok(i) => i,
        Err(e) => {
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                bundle_ids: None,
                error: Some(format!("Failed to start instproxy: {e:?}")),
            })
        }
    };

    let apps = match instproxy_client
        .get_apps(Some("User".to_string()), None)
        .await
    {
        Ok(apps) => apps,
        Err(e) => {
            info!("Failed to get apps: {:?}", e);
            return Json(GetAppsReturn {
                ok: false,
                apps: Vec::new(),
                bundle_ids: None,
                error: Some(format!("Failed to get apps: {:?}", e)),
            });
        }
    };
    let mut apps: HashMap<String, String> = apps
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
        .map(|(bundle_id, app)| {
            let name = match app {
                plist::Value::Dictionary(mut d) => match d.remove("CFBundleName") {
                    Some(plist::Value::String(bundle_name)) => bundle_name,
                    _ => bundle_id.clone(),
                },
                _ => bundle_id.clone(),
            };
            (name.clone(), bundle_id)
        })
        .collect();

    if apps.is_empty() {
        return Json(GetAppsReturn {
            ok: false,
            apps: Vec::new(),
            bundle_ids: None,
            error: Some("No apps with get-task-allow found".to_string()),
        });
    }

    apps.insert("Other...".to_string(), "UPDATE YOUR SHORTCUT".to_string());

    state
        .new_heartbeat_sender
        .send(heartbeat::SendRequest::Kill(udid.clone()))
        .await
        .unwrap();

    Json(GetAppsReturn {
        ok: true,
        apps: apps.keys().map(|x| x.to_string()).collect(),
        bundle_ids: Some(apps),
        error: None,
    })
}

#[derive(Serialize, Deserialize)]
struct LaunchAppReturn {
    ok: bool,
    launching: bool,
    position: Option<usize>,
    error: Option<String>,
    mounting: bool, // NOTICE: this field does literally nothing and will be removed in future
                    // versions
}

///  - Get the IP from the request and UDID from the database
///  - Mount the device
///  - Connect to tunneld and get the interface and port for the developer service
///  - Send the commands to launch the app and detach
///  - Set last_used to now in the database
async fn launch_app(
    ip: SecureClientIp,
    Path(bundle_id): Path<String>,
    State(state): State<JitStreamerState>,
) -> Json<LaunchAppReturn> {
    let ip = ip.0;

    info!("Got request to launch {bundle_id} from {:?}", ip);

    let udid = match common::get_udid_from_ip(ip.to_string()).await {
        Ok(u) => u,
        Err(e) => {
            return Json(LaunchAppReturn {
                ok: false,
                error: Some(e),
                launching: false,
                position: None,
                mounting: false,
            })
        }
    };

    // Get the pairing file
    debug!("Getting pairing file for {udid}");
    let pairing_file = match get_pairing_file(&udid, &state.pairing_file_storage).await {
        Ok(pairing_file) => pairing_file,
        Err(e) => {
            info!("Failed to get pairing file: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                launching: false,
                position: None,
                mounting: false,
                error: Some(format!("Failed to get pairing file: {:?}", e)),
            });
        }
    };

    // Heartbeat the device
    match heartbeat::heartbeat_thread(udid.clone(), ip, &pairing_file).await {
        Ok(s) => {
            state
                .new_heartbeat_sender
                .send(heartbeat::SendRequest::Store((udid.clone(), s)))
                .await
                .unwrap();
        }
        Err(e) => {
            let e = match e {
                idevice::IdeviceError::InvalidHostID => {
                    "your pairing file is invalid. Regenerate it with jitterbug pair.".to_string()
                }
                _ => e.to_string(),
            };
            info!("Failed to heartbeat device: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                launching: false,
                position: None,
                mounting: false,
                error: Some(format!("Failed to heartbeat device: {e}")),
            });
        }
    }

    let provider = TcpProvider {
        addr: ip,
        pairing_file,
        label: "JitStreamer-EB".to_string(),
    };

    let proxy = match CoreDeviceProxy::connect(&provider).await {
        Ok(p) => p,
        Err(e) => {
            info!("Failed to proxy device: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                launching: false,
                position: None,
                mounting: false,
                error: Some(format!("Failed to start core device proxy: {e}")),
            });
        }
    };
    let rsd_port = proxy.handshake.server_rsd_port;
    let mut adapter = match proxy.create_software_tunnel() {
        Ok(a) => a,
        Err(e) => {
            info!("Failed to create software tunnel: {:?}", e);
            return Json(LaunchAppReturn {
                ok: false,
                launching: false,
                position: None,
                mounting: false,
                error: Some(format!("Failed to create software tunnel: {e}")),
            });
        }
    };

    if let Err(e) = adapter.connect(rsd_port).await {
        info!("Failed to connect to RemoteXPC port: {:?}", e);
        return Json(LaunchAppReturn {
            ok: false,
            launching: false,
            position: None,
            mounting: false,
            error: Some(format!("Failed to connect to RemoteXPC port: {e}")),
        });
    }

    let xpc_client = match idevice::xpc::XPCDevice::new(adapter).await {
        Ok(x) => x,
        Err(e) => {
            log::warn!("Failed to connect to RemoteXPC: {e:?}");
            return Json(LaunchAppReturn {
                ok: false,
                error: Some("Failed to connect to RemoteXPC".to_string()),
                launching: false,
                position: None,
                mounting: false,
            });
        }
    };

    let dvt_port = match xpc_client.services.get(idevice::dvt::SERVICE_NAME) {
        Some(s) => s.port,
        None => {
            return Json(LaunchAppReturn {
                ok: false,
                error: Some(
                    "Device did not contain DVT service. Is the image mounted?".to_string(),
                ),
                launching: false,
                position: None,
                mounting: false,
            });
        }
    };
    let debug_proxy_port = match xpc_client.services.get(idevice::debug_proxy::SERVICE_NAME) {
        Some(s) => s.port,
        None => {
            return Json(LaunchAppReturn {
                ok: false,
                error: Some(
                    "Device did not contain debug server service. Is the image mounted?"
                        .to_string(),
                ),
                launching: false,
                position: None,
                mounting: false,
            });
        }
    };

    let mut adapter = xpc_client.into_inner();
    if let Err(e) = adapter.close().await {
        log::warn!("Failed to close RemoteXPC port: {e:?}");
        return Json(LaunchAppReturn {
            ok: false,
            error: Some("Failed to close RemoteXPC port".to_string()),
            launching: false,
            position: None,
            mounting: false,
        });
    }

    info!("Connecting to DVT port");
    if let Err(e) = adapter.connect(dvt_port).await {
        log::warn!("Failed to connect to DVT port: {e:?}");
        return Json(LaunchAppReturn {
            ok: false,
            error: Some("Failed to connect to DVT port".to_string()),
            launching: false,
            position: None,
            mounting: false,
        });
    }

    let mut rs_client = match idevice::dvt::remote_server::RemoteServerClient::new(adapter) {
        Ok(r) => r,
        Err(e) => {
            log::warn!("Failed to create remote server client: {e:?}");
            return Json(LaunchAppReturn {
                ok: false,
                error: Some(format!("Failed to create remote server client: {e:?}")),
                launching: false,
                position: None,
                mounting: false,
            });
        }
    };
    if let Err(e) = rs_client.read_message(0).await {
        log::warn!("Failed to read first message from remote server client: {e:?}");
        return Json(LaunchAppReturn {
            ok: false,
            error: Some(format!(
                "Failed to read first message from remote server client: {e:?}"
            )),
            launching: false,
            position: None,
            mounting: false,
        });
    }

    let mut pc_client =
        match idevice::dvt::process_control::ProcessControlClient::new(&mut rs_client).await {
            Ok(p) => p,
            Err(e) => {
                log::warn!("Failed to create process control client: {e:?}");
                return Json(LaunchAppReturn {
                    ok: false,
                    error: Some(format!("Failed to create process control client: {e:?}")),
                    launching: false,
                    position: None,
                    mounting: false,
                });
            }
        };

    let pid = match pc_client
        .launch_app(bundle_id, None, None, true, false)
        .await
    {
        Ok(p) => p,
        Err(e) => {
            log::warn!("Failed to launch app: {e:?}");
            return Json(LaunchAppReturn {
                ok: false,
                error: Some(format!("Failed to launch app: {e:?}")),
                launching: false,
                position: None,
                mounting: false,
            });
        }
    };
    debug!("Launched app with PID {pid}");
    if let Err(e) = pc_client.disable_memory_limit(pid).await {
        log::warn!("Failed to disable memory limit: {e:?}")
    }

    let mut adapter = rs_client.into_inner();
    if let Err(e) = adapter.close().await {
        log::warn!("Failed to close DVT port: {e:?}");
        return Json(LaunchAppReturn {
            ok: false,
            error: Some("Failed to close RemoteXPC port".to_string()),
            launching: false,
            position: None,
            mounting: false,
        });
    }

    info!("Connecting to debug proxy port: {debug_proxy_port}");
    if let Err(e) = adapter.connect(debug_proxy_port).await {
        log::warn!("Failed to connect to debug proxy port: {e:?}");
        return Json(LaunchAppReturn {
            ok: false,
            error: Some("Failed to connect to debug proxy port".to_string()),
            launching: false,
            position: None,
            mounting: false,
        });
    }

    let mut dp = DebugProxyClient::new(adapter);
    let commands = [
        format!("vAttach;{pid:02X}"),
        "D".to_string(),
        "D".to_string(),
        "D".to_string(),
        "D".to_string(),
    ];
    for command in commands {
        match dp.send_command(command.into()).await {
            Ok(res) => {
                debug!("command res: {res:?}");
            }
            Err(e) => {
                log::warn!("Failed to send command to debug server: {e:?}");
                return Json(LaunchAppReturn {
                    ok: false,
                    error: Some(format!("Failed to send command to debug server: {e:?}")),
                    launching: false,
                    position: None,
                    mounting: false,
                });
            }
        }
    }

    debug!("JIT finished, killing heartbeat");
    state
        .new_heartbeat_sender
        .send(heartbeat::SendRequest::Kill(udid.clone()))
        .await
        .unwrap();

    Json(LaunchAppReturn {
        ok: true,
        error: None,
        launching: true,   // true for compatibility reasons, will be removed
        position: Some(0), // compat field
        mounting: false,
    })
}

// compat with OG JitStreamer
#[derive(Debug, Serialize)]
struct AttachReturn {
    success: bool,
    message: String,
}

impl AttachReturn {
    fn fail(message: String) -> Self {
        Self {
            success: false,
            message,
        }
    }
}

async fn attach_app(
    ip: SecureClientIp,
    Path(pid): Path<u16>,
    State(state): State<JitStreamerState>,
) -> Json<AttachReturn> {
    let ip = ip.0;

    info!("Got request to attach {pid} from {:?}", ip);

    let udid = match common::get_udid_from_ip(ip.to_string()).await {
        Ok(u) => u,
        Err(e) => return Json(AttachReturn::fail(e)),
    };

    // Get the pairing file
    debug!("Getting pairing file for {udid}");
    let pairing_file = match get_pairing_file(&udid, &state.pairing_file_storage).await {
        Ok(pairing_file) => pairing_file,
        Err(e) => {
            info!("Failed to get pairing file: {:?}", e);
            return Json(AttachReturn::fail(format!(
                "Failed to get pairing file: {:?}",
                e
            )));
        }
    };

    // Heartbeat the device
    match heartbeat::heartbeat_thread(udid.clone(), ip, &pairing_file).await {
        Ok(s) => {
            state
                .new_heartbeat_sender
                .send(heartbeat::SendRequest::Store((udid.clone(), s)))
                .await
                .unwrap();
        }
        Err(e) => {
            let e = match e {
                idevice::IdeviceError::InvalidHostID => {
                    "your pairing file is invalid. Regenerate it with jitterbug pair.".to_string()
                }
                _ => e.to_string(),
            };
            info!("Failed to heartbeat device: {:?}", e);
            return Json(AttachReturn::fail(format!(
                "Failed to heartbeat device: {e}"
            )));
        }
    }

    let provider = TcpProvider {
        addr: ip,
        pairing_file,
        label: "JitStreamer-EB".to_string(),
    };

    let proxy = match CoreDeviceProxy::connect(&provider).await {
        Ok(p) => p,
        Err(e) => {
            info!("Failed to proxy device: {:?}", e);
            return Json(AttachReturn::fail(format!(
                "Failed to start core device proxy: {e}"
            )));
        }
    };
    let rsd_port = proxy.handshake.server_rsd_port;
    let mut adapter = match proxy.create_software_tunnel() {
        Ok(a) => a,
        Err(e) => {
            info!("Failed to create software tunnel: {:?}", e);
            return Json(AttachReturn::fail(format!(
                "Failed to create software tunnel: {e}"
            )));
        }
    };
    if let Err(e) = adapter.connect(rsd_port).await {
        info!("Failed to connect to RemoteXPC port: {:?}", e);
        return Json(AttachReturn::fail(format!(
            "Failed to connect to RemoteXPC port: {e}"
        )));
    }

    let xpc_client = match idevice::xpc::XPCDevice::new(adapter).await {
        Ok(x) => x,
        Err(e) => {
            log::warn!("Failed to connect to RemoteXPC: {e:?}");
            return Json(AttachReturn::fail(
                "Failed to connect to RemoteXPC".to_string(),
            ));
        }
    };

    let service_port = match xpc_client.services.get(idevice::debug_proxy::SERVICE_NAME) {
        Some(s) => s.port,
        None => {
            return Json(AttachReturn::fail(
                "Device did not contain debug server service. Is the image mounted?".to_string(),
            ));
        }
    };

    let mut adapter = xpc_client.into_inner();
    if let Err(e) = adapter.close().await {
        log::warn!("Failed to close RemoteXPC port: {e:?}");
        return Json(AttachReturn::fail(format!(
            "Failed to close RemoteXPC port: {e:?}"
        )));
    }
    if let Err(e) = adapter.connect(service_port).await {
        log::warn!("Failed to connect to debug proxy port: {e:?}");
        return Json(AttachReturn::fail(format!(
            "Failed to connect to debug proxy port: {e:?}"
        )));
    }

    let mut dp = DebugProxyClient::new(adapter);
    let commands = [format!("vAttach;{pid:02X}"), "D".to_string()];
    for command in commands {
        match dp.send_command(command.into()).await {
            Ok(res) => {
                debug!("command res: {res:?}");
            }
            Err(e) => {
                log::warn!("Failed to send command to debug server: {e:?}");
                return Json(AttachReturn::fail(format!(
                    "Failed to send command to debug server: {e:?}"
                )));
            }
        }
    }

    state
        .new_heartbeat_sender
        .send(heartbeat::SendRequest::Kill(udid.clone()))
        .await
        .unwrap();

    Json(AttachReturn {
        success: true,
        message: "".to_string(),
    })
}

#[derive(Debug, Serialize)]
struct StatusReturn {
    done: bool,
    ok: bool,
    position: usize,
    error: Option<String>,
    in_progress: bool, // NOTICE: this field is deprecated and will be removed in future versions
}

/// Stub function to remain compatible with dependant apps
/// Will be removed in future updates
async fn status() -> Json<StatusReturn> {
    Json(StatusReturn {
        ok: true,
        done: true,
        position: 0,
        error: None,
        in_progress: false,
    })
}

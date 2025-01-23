// Jackson Coxson
// Interface with pymobiledevice3's tunneld

use log::error;

pub async fn wait_for_connection(udid: &str, tries: u8) -> bool {
    for _ in 0..tries * 2 {
        if check_connected(udid).await {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }

    false
}

pub async fn check_connected(udid: &str) -> bool {
    // Connect to http://127.0.0.1:49151
    let client = reqwest::Client::new();
    let res = client.get("http://127.0.0.1:49151").send().await;

    let res = match res {
        Ok(res) => res,
        Err(e) => {
            error!("Unable to connect to tunneld! {e:?}");
            return false;
        }
    };

    let res = match res.json::<serde_json::Value>().await {
        Ok(res) => res,
        Err(e) => {
            error!("Tunneld returned a bad JSON res: {e:?}");
            return false;
        }
    };

    if let serde_json::Value::Object(map) = res {
        for (key, _) in map {
            if key == udid {
                return true;
            }
        }
        false
    } else {
        error!("Unexpected response format from tunneld");
        false
    }
}

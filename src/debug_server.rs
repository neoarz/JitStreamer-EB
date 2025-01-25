// Jackson Coxson
// Until a Rust implementation is written, we're gonna use a queue system to interface with a
// Python runner or two.
// See rsd.rs for my rant

use log::info;
use sqlite::State;

pub enum LaunchQueueInfo {
    Position(usize),
    NotInQueue,
    Error(String),
    ServerError,
}

// create table launch_queue (
//   udid varchar(40) not null,
//   bundle_id varchar(255) not null,
//   status int not null, -- 0: pending, 2: error
//   error varchar(255),
//   ordinal int primary key
// );

pub async fn get_queue_info(udid: &str) -> LaunchQueueInfo {
    let udid = udid.to_string();
    tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                log::error!("Failed to open database: {:?}", e);
                return LaunchQueueInfo::ServerError;
            }
        };

        // Determine the status of the UDID
        let query = "SELECT ordinal, status FROM launch_queue WHERE udid = ?";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, udid.as_str())).unwrap();
        let (ordinal, status) = if let Ok(State::Row) = statement.next() {
            let ordinal = statement.read::<i64, _>("ordinal").unwrap();
            let status = statement.read::<i64, _>("status").unwrap();
            info!(
                "Found device with ordinal {} and status {}",
                ordinal, status
            );
            (ordinal as usize, status as usize)
        } else {
            log::debug!("No device found for UDID {:?}", udid);
            return LaunchQueueInfo::NotInQueue;
        };

        match status {
            1 => return LaunchQueueInfo::Position(0),
            2 => {
                let query = "SELECT error FROM launch_queue WHERE ordinal = ?";
                let mut statement = db.prepare(query).unwrap();
                statement.bind((1, ordinal as i64)).unwrap();
                let error = if let Ok(State::Row) = statement.next() {
                    statement.read::<String, _>("error").unwrap()
                } else {
                    "Unknown error".to_string()
                };
                // Delete the record from the database
                let query = "DELETE FROM launch_queue WHERE ordinal = ?";
                let mut statement = db.prepare(query).unwrap();
                statement.bind((1, ordinal as i64)).unwrap();
                if let Err(e) = statement.next() {
                    log::error!("Failed to delete record: {:?}", e);
                }
                return LaunchQueueInfo::Error(error);
            }
            _ => {}
        }

        // Determine the position of the UDID
        let query = "SELECT COUNT(*) FROM launch_queue WHERE ordinal < ? AND status = 0";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, ordinal as i64)).unwrap();
        let position = if let Ok(State::Row) = statement.next() {
            statement.read::<i64, _>(0).unwrap()
        } else {
            return LaunchQueueInfo::ServerError;
        };

        LaunchQueueInfo::Position(position as usize)
    })
    .await
    .unwrap()
}

pub async fn add_to_queue(udid: &str, ip: String, bundle_id: &str) -> Option<i64> {
    let udid = udid.to_string();
    let bundle_id = bundle_id.to_string();
    tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                log::error!("Failed to open database: {:?}", e);
                return None;
            }
        };

        let query = "INSERT INTO launch_queue (udid, ip, bundle_id, status) VALUES (?, ?, ?, 0)";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, udid.as_str())).unwrap();
        statement.bind((2, ip.as_str())).unwrap();
        statement.bind((3, bundle_id.as_str())).unwrap();


        let mut tries = 5;
        while tries > 0 {
            if let Err(e) = statement.next() {
                log::warn!("Database is locked: {:?}", e);
                if tries == 0 {
                    return None;
                }
                tries -= 1;
                std::thread::sleep(std::time::Duration::from_millis(100));
            } else {
                break;
            }
        }

        // Get the position of the newly added UDID
        let query = "SELECT COUNT(*) FROM launch_queue WHERE ordinal < (SELECT ordinal FROM launch_queue WHERE udid = ?) AND status = 0";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, udid.as_str())).unwrap();
        if let Ok(State::Row) = statement.next() {
            Some(statement.read::<i64, _>(0).unwrap())
        } else {
            None
        }
    })
    .await
    .unwrap()
}

pub async fn empty() {
    tokio::task::spawn_blocking(|| {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                log::error!("Failed to open database: {:?}", e);
                return;
            }
        };

        let query = "DELETE FROM launch_queue";
        let mut statement = db.prepare(query).unwrap();
        statement.next().unwrap();
    })
    .await
    .unwrap();
}

// Jackson Coxson
// Abstractions for querying the mounting queue

use log::info;
use sqlite::State;

pub enum MountQueueInfo {
    Position(usize),
    InProgress,
    NotInQueue,
    Error(String),
    ServerError,
}

// create table mount_queue (
//   udid varchar(40) not null,
//   status int not null, -- 0: pending, 2: error
//   error varchar(255),
//   ordinal int primary key
// );

pub async fn get_queue_info(udid: &str) -> MountQueueInfo {
    let udid = udid.to_string();
    tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                log::error!("Failed to open database: {:?}", e);
                return MountQueueInfo::ServerError;
            }
        };

        // Determine the status of the UDID
        let query = "SELECT ordinal, status FROM mount_queue WHERE udid = ?";
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
            return MountQueueInfo::NotInQueue;
        };

        match status {
            1 => return MountQueueInfo::InProgress,
            2 => {
                let query = "SELECT error FROM mount_queue WHERE ordinal = ?";
                let mut statement = db.prepare(query).unwrap();
                statement.bind((1, ordinal as i64)).unwrap();
                let error = if let Ok(State::Row) = statement.next() {
                    statement.read::<String, _>("error").unwrap()
                } else {
                    "Unknown error".to_string()
                };
                // Delete the error from the database
                let query = "DELETE FROM mount_queue WHERE ordinal = ?";
                let mut statement = db.prepare(query).unwrap();
                statement.bind((1, ordinal as i64)).unwrap();
                if let Err(e) = statement.next() {
                    log::error!("Failed to delete error: {:?}", e);
                }
                return MountQueueInfo::Error(error);
            }
            _ => {}
        }

        // Determine the position of the UDID
        let query = "SELECT COUNT(*) FROM mount_queue WHERE ordinal < ? AND status = 0";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, ordinal as i64)).unwrap();
        let position = if let Ok(State::Row) = statement.next() {
            statement.read::<i64, _>(0).unwrap()
        } else {
            return MountQueueInfo::ServerError;
        };

        MountQueueInfo::Position(position as usize)
    })
    .await
    .unwrap()
}

pub async fn add_to_queue(udid: &str) {
    let udid = udid.to_string();
    tokio::task::spawn_blocking(move || {
        let db = match sqlite::open("jitstreamer.db") {
            Ok(db) => db,
            Err(e) => {
                log::error!("Failed to open database: {:?}", e);
                return;
            }
        };

        let query = "INSERT INTO mount_queue (udid, status) VALUES (?, 0)";
        let mut statement = db.prepare(query).unwrap();
        statement.bind((1, udid.as_str())).unwrap();
        statement.next().unwrap();
    })
    .await
    .unwrap();
}

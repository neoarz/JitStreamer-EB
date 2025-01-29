// Jackson Coxson
// Until a Rust implementation is written, we're gonna use a queue system to interface with a
// Python runner or two.
// See rsd.rs for my rant

use log::debug;
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
        let mut statement = match crate::db::db_prepare(&db, query) {
            Some(s) => s,
            None => {
                log::error!("Failed to prepare query!");
                return LaunchQueueInfo::ServerError;
            }
        };
        statement.bind((1, udid.as_str())).unwrap();
        let (ordinal, status) = if let Some(State::Row) = crate::db::statement_next(&mut statement)
        {
            let ordinal = statement.read::<i64, _>("ordinal").unwrap();
            let status = statement.read::<i64, _>("status").unwrap();
            debug!(
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
                let mut statement = match crate::db::db_prepare(&db, query) {
                    Some(s) => s,
                    None => {
                        log::error!("Failed to prepare query!");
                        return LaunchQueueInfo::ServerError;
                    }
                };
                statement.bind((1, ordinal as i64)).unwrap();
                let error = if let Some(State::Row) = crate::db::statement_next(&mut statement) {
                    statement.read::<String, _>("error").unwrap()
                } else {
                    "Unknown error".to_string()
                };
                // Delete the record from the database
                let query = "DELETE FROM launch_queue WHERE ordinal = ?";
                let mut statement = match crate::db::db_prepare(&db, query) {
                    Some(s) => s,
                    None => {
                        log::error!("Failed to prepare query!");
                        return LaunchQueueInfo::ServerError;
                    }
                };
                statement.bind((1, ordinal as i64)).unwrap();
                if crate::db::statement_next(&mut statement).is_none() {
                    log::error!("Failed to delete record");
                }
                return LaunchQueueInfo::Error(error);
            }
            _ => {}
        }

        // Determine the position of the UDID
        let query = "SELECT COUNT(*) FROM launch_queue WHERE ordinal < ? AND status = 0";
        let mut statement = match crate::db::db_prepare(&db, query) {
            Some(s) => s,
            None => {
                log::error!("Failed to prepare query!");
                return LaunchQueueInfo::ServerError;
            }
        };
        statement.bind((1, ordinal as i64)).unwrap();
        let position = if let Some(State::Row) = crate::db::statement_next(&mut statement) {
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
        let mut statement = match crate::db::db_prepare(&db, query) {
            Some(s) => s,
            None => {
                log::error!("Failed to prepare query!");
                return None;
            }
        };
        statement.bind((1, udid.as_str())).unwrap();
        statement.bind((2, ip.as_str())).unwrap();
        statement.bind((3, bundle_id.as_str())).unwrap();



        if crate::db::statement_next(&mut statement).is_none() {
            log::error!("Failed to insert into launch queue");
            return None;
        }

        // Get the position of the newly added UDID
        let query = "SELECT COUNT(*) FROM launch_queue WHERE ordinal < (SELECT ordinal FROM launch_queue WHERE udid = ?) AND status = 0";
        let mut statement = match crate::db::db_prepare(&db, query) {
            Some(s) => s,
            None => {
                log::error!("Failed to prepare query!");
                return None;
            }
        };
        statement.bind((1, udid.as_str())).unwrap();
        if let Some(State::Row) = crate::db::statement_next(&mut statement) {
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
        if crate::db::statement_next(&mut statement).is_none() {
            log::error!("Failed to empty launch queue");
        }
    })
    .await
    .unwrap();
}

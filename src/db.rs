// Jackson Coxson
// Code to retry a few times until the database isn't locked.

use sqlite::{Connection, State, Statement};

pub fn db_prepare<'a>(db: &'a Connection, query: &str) -> Option<Statement<'a>> {
    for _ in 0..50 {
        match db.prepare(query) {
            Ok(s) => return Some(s),
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    None
}

pub fn statement_next(statement: &mut Statement) -> Option<State> {
    for _ in 0..50 {
        match statement.next() {
            Ok(s) => return Some(s),
            Err(_) => {
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
        }
    }
    None
}

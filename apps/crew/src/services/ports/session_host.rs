use crate::domain::Agent; 

pub trait SessionHost {
    fn current(&self) -> Option<Agent>;
    fn is_alive(&self, name: &str) -> bool;
}

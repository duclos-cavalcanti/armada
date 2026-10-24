use super::agent::Agent;

#[derive(Clone)]
pub struct Registry {
    agents: Vec<Agent>,
}

impl Registry {
    pub fn new() -> Self {
        Self {
            agents: Vec::new(),
        }
    }

    pub fn from_agents(agents: Vec<Agent>) -> Self {
        Self {
            agents,
        }
    }

    pub fn agents(&self) -> &[Agent] {
        &self.agents
    }

    pub fn get(&self, i: usize) -> Option<Agent> {
        self.agents.get(i).cloned()
    }

    pub fn add(&mut self, agent: Agent) {
        if self.position(&agent.name).is_some() {
            return;
        }
        self.agents.push(agent);
    }

    pub fn rm(&mut self, pos: usize) -> Option<Agent> {
        if pos < self.agents.len() {
            Some(self.agents.remove(pos))
        } else {
            None
        }
    }

    pub fn position(&self, name: &str) -> Option<usize> {
        self.agents.iter().position(|a| a.name == name)
    }

    pub fn next(&self, cur: usize) -> Option<Agent> {
        let n = self.agents.len();
        if self.agents.is_empty() {
            None
        } else {
            self.agents.get((cur + 1) % n).cloned()
        }
    }

    pub fn prev(&self, cur: usize) -> Option<Agent> {
        let n = self.agents.len();
        if self.agents.is_empty() {
            None
        } else {
            self.agents.get((cur + (n - 1)) % n).cloned()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster(names: &[&str]) -> Registry {
        Registry::from_agents(names.iter().map(|n| Agent::new(*n, "/path")).collect())
    }

    #[test]
    fn add_appends_new_name() {
        let mut r = Registry::new();
        r.add(Agent::new("millwright", "/a"));
        r.add(Agent::new("prover", "/b"));
        assert_eq!(r.agents().len(), 2);
    }

    #[test]
    fn add_dedups_by_name() {
        let mut r = roster(&["millwright"]);
        r.add(Agent::new("millwright", "/other"));
        assert_eq!(r.agents().len(), 1);
    }

    #[test]
    fn rm_in_bounds_removes_and_returns_it() {
        let mut r = roster(&["millwright", "prover"]);
        let removed = r.rm(0);
        assert_eq!(removed.map(|a| a.name), Some("millwright".to_string()));
        assert_eq!(r.agents().len(), 1);
        assert_eq!(r.agents()[0].name, "prover");
    }

    #[test]
    fn rm_out_of_bounds_is_none_and_no_change() {
        let mut r = roster(&["millwright"]);
        assert!(r.rm(5).is_none());
        assert_eq!(r.agents().len(), 1);
    }

    #[test]
    fn position_finds_name_or_none() {
        let r = roster(&["millwright", "prover"]);
        assert_eq!(r.position("prover"), Some(1));
        assert_eq!(r.position("ghost"), None);
    }

    #[test]
    fn next_advances_and_wraps() {
        let r = roster(&["a", "b", "c"]);
        assert_eq!(r.next(0).map(|a| a.name), Some("b".to_string()));
        assert_eq!(r.next(2).map(|a| a.name), Some("a".to_string())); // wraps to first
    }

    #[test]
    fn prev_retreats_and_wraps() {
        let r = roster(&["a", "b", "c"]);
        assert_eq!(r.prev(2).map(|a| a.name), Some("b".to_string()));
        assert_eq!(r.prev(0).map(|a| a.name), Some("c".to_string())); // wraps to last
    }

    #[test]
    fn nav_on_empty_registry_is_none() {
        let r = Registry::new();
        assert!(r.next(0).is_none());
        assert!(r.prev(0).is_none());
    }
}

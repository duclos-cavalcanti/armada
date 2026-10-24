#[derive(Clone)]
pub struct Agent {
    pub name: String,
    pub path: String,
}

impl Agent {
    pub fn new(name: impl Into<String>, path: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            path: path.into(),
        }
    }
}

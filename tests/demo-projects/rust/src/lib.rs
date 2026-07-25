pub struct Greeter {
    prefix: &'static str,
}

impl Greeter {
    pub const fn new(prefix: &'static str) -> Self {
        Self { prefix }
    }

    pub fn greet(&self, name: &str) -> String {
        format!("{}, {name}!", self.prefix)
    }
}

//! The Supervisor Host: holds the loaded Supervisor plugins (skeleton).
//!
//! One process, many Supervisors. For now the host only registers plugins and reports how many it
//! holds; running them — each as its own OpAMP Agent against the Server — is a later change (ADR-0005).

use crate::ports::Supervisor;

/// A single process that hosts multiple [`Supervisor`] plugins.
#[derive(Default)]
pub struct SupervisorHost {
    supervisors: Vec<Box<dyn Supervisor>>,
}

impl SupervisorHost {
    /// A host with no plugins registered yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers one Supervisor plugin with the host.
    pub fn register(&mut self, supervisor: Box<dyn Supervisor>) {
        self.supervisors.push(supervisor);
    }

    /// The Supervisor plugins currently registered.
    pub fn supervisors(&self) -> &[Box<dyn Supervisor>] {
        &self.supervisors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Dummy(&'static str);
    impl Supervisor for Dummy {
        fn name(&self) -> &str {
            self.0
        }
    }

    #[test]
    fn registers_and_lists_plugins() {
        let mut host = SupervisorHost::new();
        assert_eq!(host.supervisors().len(), 0);
        host.register(Box::new(Dummy("collector")));
        host.register(Box::new(Dummy("custom:nginx")));
        assert_eq!(host.supervisors().len(), 2);
        assert_eq!(host.supervisors()[0].name(), "collector");
    }
}

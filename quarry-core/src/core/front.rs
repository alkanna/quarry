//! The UI holds the core through `Front`: it reads any state, but changes it only through these doors, so a click
//! cannot skip the action path that tests and agents go through.

use super::Core;
use crate::action::agents::{AgentCall, Origin};
use crate::action::{Action, Refusal, Reply, UiFx};
use crate::db::Msg;
use std::ops::Deref;

pub struct Front(Core, bool);

impl Deref for Front {
    type Target = Core;
    fn deref(&self) -> &Core {
        &self.0
    }
}

impl Front {
    /// Takes a core once it is built, demo data included; it is not writable from outside after this.
    pub fn new(core: Core) -> Self {
        Front(core, false)
    }

    /// Startup only: the demo builders and test switches fill the core before the first frame. Panics after that,
    /// so drawing code cannot use it to skip the action path.
    pub fn setup(&mut self) -> &mut Core {
        assert!(!self.1, "Front::setup after the first tick");
        &mut self.0
    }

    pub fn execute(&mut self, a: Action) -> Result<Reply, Refusal> {
        self.0.execute(a)
    }

    pub fn execute_as(&mut self, origin: Origin, a: AgentCall) {
        self.0.execute_as(origin, a)
    }

    pub fn on_msg(&mut self, m: Msg) {
        self.0.on_msg(m)
    }

    pub fn tick(&mut self) -> Vec<Msg> {
        self.1 = true;
        self.0.tick()
    }

    pub fn take_ui_fx(&mut self) -> Vec<UiFx> {
        self.0.take_ui_fx()
    }

    /// Tests set up state directly; release builds have no such door.
    #[cfg(any(test, feature = "test-support"))]
    pub fn test_mut(&mut self) -> &mut Core {
        &mut self.0
    }
}

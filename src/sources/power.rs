//! Power actions. The defaults target OpenRC + elogind rather than systemd.
//!
//! Every command is configurable, to absorb the differences between elogind, systemd and doas.

use crate::util::proc;

use crate::config::{CommandsConfig, PowerConfig};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerAction {
    ScreenOff,
    Suspend,
    PowerOff,
}

impl PowerAction {
    pub fn label(self) -> &'static str {
        match self {
            PowerAction::ScreenOff => "Screen Off",
            PowerAction::Suspend => "Suspend",
            PowerAction::PowerOff => "Power Off",
        }
    }

    /// The wording shown in the confirmation modal.
    pub fn question(self) -> &'static str {
        match self {
            PowerAction::ScreenOff => "Turn the screen off?",
            PowerAction::Suspend => "Suspend this machine?",
            PowerAction::PowerOff => "Power off this machine?",
        }
    }

    /// Whether the confirmation may be skipped.
    ///
    /// **Suspend and Power Off can never be skipped, not even by config.** Setting one off by accident costs too much.
    pub fn may_skip_confirm(self, config: &PowerConfig) -> bool {
        matches!(self, PowerAction::ScreenOff) && !config.confirm_screen_off
    }

    fn argv(self, config: &PowerConfig) -> &[String] {
        match self {
            PowerAction::ScreenOff => &config.screen_off,
            PowerAction::Suspend => &config.suspend,
            PowerAction::PowerOff => &config.poweroff,
        }
    }
}

/// Run the configured command.
///
/// Passed as an argv array, never through a shell, so shell metacharacters in the config are not interpreted.
///
/// **Waits `config.delay_ms` before executing.**
/// The release event of the Enter key or mouse button you just pressed would
/// otherwise undo the suspend or DPMS immediately (the Hyprland config here has
/// `key_press_enables_dpms` and `mouse_move_enables_dpms` on).
/// **This function blocks. Call it from a dedicated thread.**
pub fn run(
    action: PowerAction,
    config: &PowerConfig,
    commands: &CommandsConfig,
) -> Result<(), String> {
    if config.delay_ms > 0 {
        std::thread::sleep(std::time::Duration::from_millis(config.delay_ms));
    }
    run_now(action, config, commands)
}

/// Run without the delay: for tests, and for callers that own the delay themselves.
pub fn run_now(
    action: PowerAction,
    config: &PowerConfig,
    commands: &CommandsConfig,
) -> Result<(), String> {
    let argv = action.argv(config);
    let Some((program, args)) = argv.split_first() else {
        return Err(format!("no command configured for {}", action.label()));
    };
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    // Suspend can block until resume, so it gets a longer budget than the rest.
    proc::status(program, &args, commands.power_timeout())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn screen_off_may_skip_confirm_by_default() {
        let c = PowerConfig::default();
        assert!(!c.confirm_screen_off);
        assert!(PowerAction::ScreenOff.may_skip_confirm(&c));
    }

    #[test]
    fn screen_off_confirmation_can_be_turned_on() {
        let c = PowerConfig {
            confirm_screen_off: true,
            ..PowerConfig::default()
        };
        assert!(!PowerAction::ScreenOff.may_skip_confirm(&c));
    }

    /// **Not skippable, not even by config.** Loosening this loses work to a misclick.
    #[test]
    fn suspend_and_poweroff_always_confirm() {
        for c in [
            PowerConfig::default(),
            PowerConfig {
                confirm_screen_off: false,
                ..PowerConfig::default()
            },
        ] {
            assert!(!PowerAction::Suspend.may_skip_confirm(&c));
            assert!(!PowerAction::PowerOff.may_skip_confirm(&c));
        }
    }

    #[test]
    fn default_commands_match_the_openrc_elogind_environment() {
        let c = PowerConfig::default();
        assert_eq!(c.suspend, ["loginctl", "suspend"]);
        assert_eq!(c.poweroff, ["loginctl", "poweroff"]);
        // The defaults target OpenRC + elogind, so systemctl is not used.
        assert!(!c.suspend.iter().any(|a| a.contains("systemctl")));
        assert_eq!(c.screen_off[0], "hyprctl");
        // The default is the upstream Hyprland spelling; any other form goes in the config.
        assert_eq!(c.screen_off, ["hyprctl", "dispatch", "dpms", "off"]);
    }

    #[test]
    fn empty_command_is_an_error_not_a_panic() {
        let c = PowerConfig {
            suspend: vec![],
            ..PowerConfig::default()
        };
        assert!(run_now(PowerAction::Suspend, &c, &CommandsConfig::default()).is_err());
    }

    /// **A zero delay causes accidents.** The default must wait one second.
    #[test]
    fn there_is_a_delay_before_executing() {
        assert_eq!(PowerConfig::default().delay_ms, 1000);
    }

    #[test]
    fn delay_actually_elapses() {
        let c = PowerConfig {
            delay_ms: 120,
            suspend: vec![],
            ..PowerConfig::default()
        };
        let t0 = std::time::Instant::now();
        let _ = run(PowerAction::Suspend, &c, &CommandsConfig::default());
        assert!(t0.elapsed() >= std::time::Duration::from_millis(120));
    }

    #[test]
    fn labels_and_questions_are_present() {
        for a in [
            PowerAction::ScreenOff,
            PowerAction::Suspend,
            PowerAction::PowerOff,
        ] {
            assert!(!a.label().is_empty());
            assert!(a.question().ends_with('?'));
        }
    }
}

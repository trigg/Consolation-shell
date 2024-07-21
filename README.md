# Consolation Shell

Consolation shell is a collection of (currently one) programs to attempt to bring up Consolation as a usable gaming environment

## Installation

### From Sources

```
git clone https://github.com/trigg/Consolation-shell
cd Consolation-shell
cargo build --release
```

## Running

Binaries compile to `./target/release` and can be run directly. To make sensible use of them, they should be added to the users `PATH` before starting [Consolation](https://github.com/trigg/Consolation)


# Features

Current features:

- Switcher
- - On start up shows a list of open windows using zwlr_foreign_toplevel_manager
- - Has buttons to activate, toggle maximise, close for each window
- - Shows window icon and title. Sometimes.

Future and hopes:

- Desktop
- - Allow for user to choose background image/colour
- - Potentially allow tie-in to currently opened window to use app-themed assets
- Notification Manager
- - Add extra notifications for important system events
- - - Battery low warnings
- - - Plugged in & Not charging
- - Store notification and place in a screen of switcher to allow user interactions where necessary
- Launcher
- - List Applications, Categories
- Settings
- - Allow changing of both Consolation and shell config from gui
- Work out how to catch attempted re-runs and alert the already running instance

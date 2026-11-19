import QtQuick
import Quickshell
import Quickshell.Io
import Quickshell.Wayland
import qs.Commons

// Runs the omaCortex recorder for as long as the shell runs, tells it when
// you step away, and gives keybindings an IPC target:
//
//   o.bind("SUPER + ALT + P", "Pause omaCortex", { ipc = "omacortex.toggle" })
//   o.bind("SUPER + ALT + O", "omaCortex panel", { ipc = "io.github.anonb3ll.omacortex.toggle" })
//
// If a systemd user unit already runs the recorder, the daemon started here
// sees the lock, exits 0, and is not restarted.
Item {
  id: root

  property var shell: null
  property var manifest: null

  readonly property string cli: decodeURIComponent(String(Qt.resolvedUrl("bin/omacortex")).replace(/^file:\/\//, ""))
  property int restarts: 0

  function run(args) {
    Quickshell.execDetached([root.cli].concat(args))
  }

  Process {
    id: daemon
    command: [root.cli, "daemon", "--if-not-running"]
    running: true
    onExited: function(exitCode) {
      // 0: stopped cleanly or another recorder holds the lock. Anything else is
      // a crash: retry with backoff, then give up until the shell reloads.
      if (exitCode !== 0 && root.restarts < 5) restartTimer.start()
    }
  }

  Timer {
    id: restartTimer
    interval: 5000 * Math.pow(2, root.restarts)
    onTriggered: {
      root.restarts++
      daemon.running = true
    }
  }

  // Away after five minutes without input. Idle inhibitors (a video playing
  // full screen) keep you "here", which is usually right: you are watching.
  IdleMonitor {
    id: idle
    timeout: 300
    respectInhibitors: true
    onIsIdleChanged: root.run(["mark", idle.isIdle ? "away" : "back"])
  }

  ShellIpc {
    target: "omacortex"

    function toggle(): void { root.run(["toggle"]) }
    function pause(): void { root.run(["pause"]) }
    function pauseHour(): void { root.run(["pause", "1h"]) }
    function resume(): void { root.run(["resume"]) }
    function forgetRecent(): void { root.run(["forget", "--last", "15m", "--yes"]) }
    function summarize(): void { root.run(["summarize", "--refresh"]) }
  }
}

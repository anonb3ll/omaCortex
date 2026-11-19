import QtQuick
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// omaCortex bar icon and "Where was I?" panel.
//
// All data comes from `bin/omacortex panel-json`, one bounded JSON document
// read from the local database. Every recorded string (window titles, track
// names, file names) is untrusted and is drawn with Text.PlainText.
Panel {
  id: root
  moduleName: "io.github.anonb3ll.omacortex"
  ipcTarget: "io.github.anonb3ll.omacortex"

  readonly property string cli: decodeURIComponent(String(Qt.resolvedUrl("bin/omacortex")).replace(/^file:\/\//, ""))
  readonly property int refreshSeconds: Math.max(10, setting("refreshSeconds", 60))

  property var info: null
  property bool summarizing: false
  property string flash: ""
  property real now: Date.now() / 1000

  readonly property var head: Model.headline(root.info, root.now)
  readonly property bool hasConsent: !!(root.info && root.info.status.consent)
  readonly property bool isPaused: !!(root.info && root.info.status.paused)
  readonly property var sessions: Model.sessionRows(root.info)
  readonly property var media: Model.mediaRows(root.info)
  readonly property var files: Model.fileRows(root.info)
  readonly property var notes: Model.noteRows(root.info)

  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property color urgent: bar ? bar.urgent : Commons.Color.urgent
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property color muted: Qt.darker(foreground, 1.4)

  function refresh() {
    if (!panelProc.running) panelProc.running = true
  }

  function act(args, message) {
    root.flash = message || ""
    actionProc.command = [root.cli].concat(args)
    actionProc.running = true
  }

  function summarize() {
    if (root.summarizing) return
    root.summarizing = true
    summaryProc.running = true
  }

  function statusText() {
    var key = root.head.key
    if (key === "paused-until") return strings.t("status.paused-until").arg(Model.clock(root.info.status.paused_until))
    if (key === "recording") {
      var total = Model.totalFocus(root.info)
      return total > 0 ? strings.t("status.focus-today").arg(Model.duration(total)) : strings.t("status.recording")
    }
    return strings.t("status." + key)
  }

  onOpenedChanged: if (opened) { root.flash = ""; refresh() }

  Commons.Translations {
    id: strings
    directory: Qt.resolvedUrl("i18n")
  }

  Process {
    id: panelProc
    command: [root.cli, "panel-json"]
    stdout: StdioCollector {
      waitForEnd: true
      onStreamFinished: {
        var parsed = Model.parse(text)
        if (parsed) root.info = parsed
        root.now = Date.now() / 1000
      }
    }
  }

  Process {
    id: actionProc
    onExited: root.refresh()
  }

  Process {
    id: summaryProc
    command: [root.cli, "summarize", "--refresh"]
    onExited: {
      root.summarizing = false
      root.refresh()
    }
  }

  // The bar icon only needs the recorder state: refresh rarely while closed.
  Timer { interval: 3000; running: true; onTriggered: root.refresh() }
  Timer {
    interval: (root.opened ? 10 : root.refreshSeconds) * 1000
    running: true
    repeat: true
    onTriggered: root.refresh()
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: root.isPaused ? "󰏥" : "󰧑"
    opacity: root.head.key === "recording" || root.head.key === "away" ? 1 : 0.55
    active: root.head.urgent
    tooltipText: root.opened ? "" : Model.tooltip(root.info, root.now)
    onPressed: function(b) {
      if (b === Qt.RightButton) root.act([root.isPaused || !root.hasConsent ? "resume" : "pause"])
      else root.toggle()
    }
  }

  component Caption: Text {
    textFormat: Text.PlainText
    color: root.muted
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    elide: Text.ElideRight
  }

  component Body: Text {
    textFormat: Text.PlainText
    color: root.foreground
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
    elide: Text.ElideRight
  }

  // time · label · detail · length, one line.
  component ActivityRow: Item {
    id: row
    property string time: ""
    property string label: ""
    property string detail: ""
    property string length: ""
    property bool live: false
    width: parent ? parent.width : 0
    implicitHeight: Math.max(labelText.implicitHeight, Style.space(20))

    Caption {
      id: timeText
      anchors.left: parent.left
      anchors.verticalCenter: parent.verticalCenter
      width: Style.space(42)
      text: row.time
    }
    Body {
      id: labelText
      anchors.left: timeText.right
      anchors.right: lengthText.left
      anchors.rightMargin: Style.space(8)
      anchors.verticalCenter: parent.verticalCenter
      text: row.detail ? row.label + "  ·  " + row.detail : row.label
      font.bold: row.live
    }
    Caption {
      id: lengthText
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      text: row.length
      color: row.live ? root.foreground : root.muted
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: keyCatcher
    contentWidth: panel.fittedContentWidth(Style.space(400))
    contentHeight: panel.fittedContentHeight(column.implicitHeight)

    PanelKeyCatcher {
      id: keyCatcher
      anchors.fill: parent
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }

      Column {
        id: column
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.top: parent.top
        spacing: Style.space(12)

        // ---------- Hero ----------
        Item {
          width: parent.width
          implicitHeight: Math.max(heroIcon.implicitHeight, heroLabels.implicitHeight)

          Text {
            id: heroIcon
            textFormat: Text.PlainText
            text: root.isPaused ? "󰏥" : "󰧑"
            color: root.head.urgent ? root.urgent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.display
            anchors.left: parent.left
            anchors.verticalCenter: parent.verticalCenter
          }

          Column {
            id: heroLabels
            anchors.left: heroIcon.right
            anchors.leftMargin: Style.space(14)
            anchors.right: heroActions.left
            anchors.rightMargin: Style.space(10)
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(2)

            Text {
              textFormat: Text.PlainText
              text: strings.t("title")
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.title
              font.bold: true
              width: parent.width
              elide: Text.ElideRight
            }
            Text {
              textFormat: Text.PlainText
              text: root.statusText().toUpperCase()
              color: root.head.urgent ? root.urgent : root.muted
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              font.letterSpacing: 1.2
              width: parent.width
              elide: Text.ElideRight
            }
          }

          Row {
            id: heroActions
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.space(4)
            visible: root.hasConsent

            PanelActionButton {
              iconText: root.isPaused ? "󰐊" : "󰏤"
              tooltipText: root.isPaused ? strings.t("action.resume") : strings.t("action.pause")
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: root.act([root.isPaused ? "resume" : "pause"])
            }
            PanelActionButton {
              iconText: "󰑐"
              tooltipText: strings.t("action.refresh")
              foreground: root.foreground
              fontFamily: root.fontFamily
              onClicked: root.refresh()
            }
          }
        }

        // ---------- First run: consent ----------
        Column {
          width: parent.width
          spacing: Style.space(10)
          visible: root.info !== null && !root.hasConsent

          Text {
            textFormat: Text.PlainText
            text: strings.t("consent.heading")
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            width: parent.width
            wrapMode: Text.Wrap
          }
          Text {
            textFormat: Text.PlainText
            text: strings.t("consent.body")
            color: root.muted
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            width: parent.width
            wrapMode: Text.Wrap
          }
          Button {
            text: strings.t("consent.start")
            iconText: "󰑋"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            onClicked: root.act(["enable"])
          }
        }

        // ---------- Now ----------
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.hasConsent

          PanelSectionHeader { text: strings.t("section.now"); foreground: root.foreground; fontFamily: root.fontFamily }
          ActivityRow {
            readonly property var cur: root.info ? root.info.current : null
            time: cur ? Model.clock(cur.started_at) : ""
            label: cur ? Model.appName(cur.app) : strings.t("now.nothing")
            detail: cur ? Model.shortTitle(cur.title, cur.app, 60) : ""
            length: cur ? Model.duration(cur.seconds) : ""
            live: !!cur
          }
        }

        PanelSeparator { foreground: root.foreground; visible: root.hasConsent }

        // ---------- Today ----------
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.hasConsent

          PanelSectionHeader { text: strings.t("section.today"); foreground: root.foreground; fontFamily: root.fontFamily }
          Caption { visible: root.sessions.length === 0; text: strings.t("today.empty"); width: parent.width }
          Repeater {
            model: root.sessions
            ActivityRow {
              required property var modelData
              time: modelData.time
              label: modelData.app
              detail: modelData.title
              length: modelData.length
              live: modelData.ongoing
            }
          }
        }

        // ---------- Media ----------
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.hasConsent && root.media.length > 0

          PanelSectionHeader { text: strings.t("section.media"); foreground: root.foreground; fontFamily: root.fontFamily }
          Repeater {
            model: root.media
            ActivityRow {
              required property var modelData
              time: modelData.time
              label: modelData.title
              detail: modelData.source
              length: modelData.ongoing ? strings.t("playing") : modelData.length
              live: modelData.ongoing
            }
          }
        }

        // ---------- Files ----------
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.hasConsent && root.files.length > 0

          PanelSectionHeader { text: strings.t("section.files"); foreground: root.foreground; fontFamily: root.fontFamily }
          Repeater {
            model: root.files
            ActivityRow {
              required property var modelData
              time: modelData.time
              label: modelData.title
            }
          }
        }

        // ---------- Notes (yours and your agents') ----------
        Column {
          width: parent.width
          spacing: Style.space(4)
          visible: root.hasConsent && root.notes.length > 0

          PanelSectionHeader { text: strings.t("section.notes"); foreground: root.foreground; fontFamily: root.fontFamily }
          Repeater {
            model: root.notes
            ActivityRow {
              required property var modelData
              time: modelData.time
              label: modelData.title
            }
          }
        }

        // ---------- Summary ----------
        Column {
          width: parent.width
          spacing: Style.space(6)
          visible: root.hasConsent

          PanelSectionHeader { text: strings.t("section.summary"); foreground: root.foreground; fontFamily: root.fontFamily }
          Text {
            textFormat: Text.PlainText
            text: root.summarizing ? strings.t("summary.working")
              : (root.info && root.info.summary ? root.info.summary : strings.t("summary.empty"))
            color: root.info && root.info.summary && !root.summarizing ? root.foreground : root.muted
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            width: parent.width
            wrapMode: Text.Wrap
            maximumLineCount: 12
            elide: Text.ElideRight
          }
        }

        PanelSeparator { foreground: root.foreground; visible: root.hasConsent }

        // ---------- Actions ----------
        Flow {
          width: parent.width
          spacing: Style.space(6)
          visible: root.hasConsent

          Button {
            text: strings.t("action.summarize")
            iconText: "󰚩"
            iconSpinning: root.summarizing
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            enabled: !root.summarizing
            onClicked: root.summarize()
          }
          Button {
            text: strings.t("action.pause-hour")
            iconText: "󱫞"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            onClicked: root.act(["pause", "1h"])
          }
          Button {
            text: strings.t("action.forget")
            iconText: "󱕖"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            onClicked: root.act(["forget", "--last", "15m", "--yes"], strings.t("action.forgot"))
          }
        }

        Caption {
          visible: root.flash !== ""
          text: root.flash
          width: parent.width
        }
      }
    }
  }
}

import QtQuick
import QtQuick.Controls
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// Widget da barra do omarchy-drive. Só lê $XDG_RUNTIME_DIR/omarchy-drive/status.json
// (escrito pelo daemon) e chama a CLI local; não acessa a rede.
Panel {
  id: root
  moduleName: "omarchy-drive.status"
  ipcTarget: "omarchy-drive.status"

  readonly property string statusPath: Quickshell.env("XDG_RUNTIME_DIR") + "/omarchy-drive/status.json"
  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omarchy-drive"

  property bool running: false
  property string overall: "idle"
  property var pairs: []

  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property color urgent: bar ? bar.urgent : Commons.Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property bool allPaused: running && pairs.length > 0 && overall === "paused"

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  function stateText(s) {
    return running ? strings.t("state." + s) : strings.t("state.stopped")
  }

  function reload() {
    var parsed = Model.parse(statusFile.text())
    running = parsed !== null
    overall = parsed ? parsed.overall : "idle"
    pairs = parsed ? parsed.pairs : []
  }

  function run(args) {
    Quickshell.execDetached([root.cli].concat(args))
  }

  function syncAll() { run(["sync"]) }
  function togglePause() { run([allPaused ? "resume" : "pause"]) }
  function openFirst() { if (pairs.length > 0) run(["open", pairs[0].name]) }
  function confirmHeld() {
    for (var i = 0; i < pairs.length; i++)
      if (pairs[i].pendingDeletes > 0) run(["confirm", pairs[i].name])
  }

  function relativeTime(iso) {
    if (!iso) return ""
    var d = new Date(iso)
    if (isNaN(d.getTime())) return ""
    return d.toLocaleTimeString(Qt.locale(Commons.I18n.formatLocale), Locale.ShortFormat)
  }

  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  FileView {
    id: statusFile
    path: root.statusPath
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.reload()
    onLoadFailed: root.reload()
  }

  // O daemon troca o arquivo por rename atômico; a releitura periódica cobre
  // reinícios do daemon e casos em que o inotify perde o arquivo.
  Timer {
    interval: root.overall === "syncing" ? 1000 : 5000
    running: true
    repeat: true
    onTriggered: statusFile.reload()
  }

  BarIconButton {
    id: button
    anchors.fill: parent
    bar: root.bar
    text: Model.glyph(root.overall, root.running)
    foreground: root.running && root.overall !== "paused" ? root.barForeground : Qt.darker(root.barForeground, 1.55)
    onPressed: function(buttonCode) {
      if (buttonCode === Qt.RightButton) root.syncAll()
      else if (buttonCode === Qt.MiddleButton) root.togglePause()
      else root.toggle()
    }
  }

  KeyboardPanel {
    id: panel
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: panel.loadedContent
    contentWidth: panel.fittedContentWidth(Style.space(380))
    contentHeight: panel.fittedContentHeight(panel.loadedContentHeight, Style.space(520))

    content: PanelKeyCatcher {
      implicitHeight: column.implicitHeight
      anchors.fill: parent
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onTextKey: function(t) {
        var k = t.toLowerCase()
        if (k === "s") root.syncAll()
        else if (k === "p") root.togglePause()
        else if (k === "o") root.openFirst()
        else if (k === "c") root.confirmHeld()
      }

      Flickable {
        anchors.fill: parent
        contentWidth: width
        contentHeight: column.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        interactive: contentHeight > height
        ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

        Column {
          id: column
          width: parent.width
          spacing: Style.space(12)

          PanelHero {
            width: parent.width
            title: strings.t("title")
            meta: root.stateText(root.overall)
            foreground: root.foreground
            fontFamily: root.fontFamily
            iconOpacity: root.running ? 1.0 : 0.5
            iconComponent: Component {
              Text {
                textFormat: Text.PlainText
                text: Model.glyph(root.overall, root.running)
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.display
              }
            }
            trailingControl: Component {
              Row {
                visible: root.running && root.pairs.length > 0
                spacing: Style.space(4)
                PanelActionButton {
                  iconText: "\u{F04E6}"
                  tooltipText: strings.t("syncNow")
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: root.syncAll()
                }
                PanelActionButton {
                  iconText: root.allPaused ? "\u{F040A}" : "\u{F03E4}"
                  tooltipText: root.allPaused ? strings.t("resume") : strings.t("pause")
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: root.togglePause()
                }
              }
            }
          }

          PlainLabel {
            visible: !root.running || root.pairs.length === 0
            text: root.running ? strings.t("noPairs") : strings.t("stoppedHint")
            color: root.dim
          }

          Repeater {
            model: root.pairs

            Column {
              required property var modelData
              width: column.width
              spacing: Style.spacing.labelGap

              PanelSeparator { foreground: root.foreground }

              Row {
                width: parent.width
                spacing: Style.space(8)
                PlainLabel {
                  width: parent.width - openButton.width - Style.space(8)
                  text: modelData.name + "  ·  " + modelData.provider + "  ·  " + root.stateText(modelData.state)
                  color: root.foreground
                  font.bold: true
                }
                PanelActionButton {
                  id: openButton
                  iconText: "\u{F0770}"
                  tooltipText: strings.t("openFolder")
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  onClicked: root.run(["open", modelData.name])
                }
              }
              PlainLabel { text: modelData.local; color: root.dim }
              PlainLabel {
                visible: modelData.lastSync !== ""
                text: strings.t("lastSync").arg(root.relativeTime(modelData.lastSync))
                color: root.dim
              }
              PlainLabel {
                visible: modelData.queued > 0
                text: strings.tn("queued", modelData.queued).arg(Commons.I18n.formatNumber(modelData.queued))
                  + (modelData.active.length > 0 ? "  ·  " + modelData.active.join(", ") : "")
                color: root.foreground
              }
              PlainLabel {
                visible: modelData.quotaTotal > 0
                text: strings.t("storage")
                  .arg(Model.humanBytes(modelData.quotaUsed, Commons.I18n.formatNumber))
                  .arg(Model.humanBytes(modelData.quotaTotal, Commons.I18n.formatNumber))
                color: root.dim
              }
              PlainLabel {
                visible: modelData.pendingDeletes > 0
                text: strings.tn("heldDeletes", modelData.pendingDeletes).arg(Commons.I18n.formatNumber(modelData.pendingDeletes))
                color: root.urgent
              }
              PlainLabel {
                visible: modelData.conflicts > 0
                text: strings.tn("conflicts", modelData.conflicts).arg(Commons.I18n.formatNumber(modelData.conflicts))
                color: root.urgent
              }
              PlainLabel {
                visible: modelData.lastError !== "" && (modelData.state === "error" || modelData.state === "offline")
                text: modelData.lastError
                color: root.urgent
              }
            }
          }
        }
      }
    }
  }

  // Todo texto dinâmico é PlainText: nomes de arquivo e erros vêm de fora.
  component PlainLabel: Text {
    width: parent ? parent.width : implicitWidth
    textFormat: Text.PlainText
    wrapMode: Text.WordWrap
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
  }
}

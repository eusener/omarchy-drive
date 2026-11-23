import QtQuick
import QtQuick.Controls
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// Widget da barra do omarchy-drive. Só lê $XDG_RUNTIME_DIR/omarchy-drive/status.json
// e auth.json (escritos pelo daemon e pela CLI) e chama a CLI local; não acessa a rede.
Panel {
  id: root
  moduleName: "omarchy-drive.status"
  ipcTarget: "omarchy-drive.status"

  readonly property string runtimeDir: Quickshell.env("XDG_RUNTIME_DIR") + "/omarchy-drive"
  readonly property string statusPath: runtimeDir + "/status.json"
  readonly property string authPath: runtimeDir + "/auth.json"
  readonly property string cli: Quickshell.env("HOME") + "/.local/bin/omarchy-drive"

  property bool running: false
  property string overall: "idle"
  property var pairs: []
  property var accounts: []
  // Daemons antigos (status.json v1) não informam as contas.
  property bool accountsKnown: false
  // Fluxo de login (auth.json) e se o processo dele ainda existe.
  property var auth: null
  property bool authAlive: false
  property real nowMs: Date.now()
  // Conta aguardando confirmação para desconectar.
  property string confirmRemove: ""

  readonly property color foreground: bar ? bar.foreground : Commons.Color.foreground
  readonly property color urgent: bar ? bar.urgent : Commons.Color.urgent
  readonly property color dim: Qt.darker(foreground, 1.55)
  readonly property string fontFamily: bar ? bar.fontFamily : Style.font.family
  readonly property bool allPaused: running && pairs.length > 0 && overall === "paused"
  readonly property bool authorizing: auth !== null && auth.state === "authorizing" && authAlive
  // Resultado recente (10 min) de um fluxo que terminou ou cujo processo morreu.
  readonly property bool authNotice: auth !== null && !authorizing && nowMs - auth.startedMs < 600000
  readonly property var firstAccount: accounts.length > 0 ? accounts[0] : null

  Commons.Translations { id: strings; directory: Qt.resolvedUrl("i18n") }

  function stateText(s) {
    return running ? strings.t("state." + s) : strings.t("state.stopped")
  }

  function reload() {
    var parsed = Model.parse(statusFile.text())
    running = parsed !== null
    overall = parsed ? parsed.overall : "idle"
    pairs = parsed ? parsed.pairs : []
    accounts = parsed ? parsed.accounts : []
    accountsKnown = parsed ? parsed.accountsKnown : false
  }

  function reloadAuth() {
    auth = Model.parseAuth(authFile.text())
    nowMs = Date.now()
  }

  function authNoticeText() {
    if (!auth) return ""
    if (auth.state === "done") return auth.message
    if (auth.state === "failed") return strings.t("authFailed").arg(auth.message)
    // Cancelado, ou o processo do login morreu sem concluir.
    return strings.t("authCancelled")
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

  function connect(provider) { if (!authorizing) run(["account", "connect", "--provider", provider]) }
  function reauth(name) { if (!authorizing) run(["account", "reauth", name]) }
  function disconnect(name) {
    confirmRemove = ""
    run(["account", "remove", name, "--with-pairs"])
  }
  function addDefaultPair(acc) {
    var gd = acc.provider === "gdrive"
    run(["pair", "add", gd ? "drive" : "onedrive", "--account", acc.name, "--local", gd ? "~/GoogleDrive" : "~/OneDrive"])
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

  FileView {
    id: authFile
    path: root.authPath
    watchChanges: true
    printErrors: false
    onFileChanged: reload()
    onLoaded: root.reloadAuth()
    onLoadFailed: root.reloadAuth()
  }

  // O processo do login existe enquanto /proc/<pid> existir.
  FileView {
    id: authProc
    path: root.auth && root.auth.state === "authorizing" && root.auth.pid > 0 ? "/proc/" + root.auth.pid + "/cmdline" : ""
    printErrors: false
    onLoaded: root.authAlive = true
    onLoadFailed: root.authAlive = false
  }

  // O daemon troca os arquivos por rename atômico; a releitura periódica cobre
  // reinícios do daemon e casos em que o inotify perde o arquivo.
  Timer {
    interval: root.overall === "syncing" || root.authorizing ? 1000 : 5000
    running: true
    repeat: true
    onTriggered: {
      statusFile.reload()
      authFile.reload()
      if (authProc.path !== "") authProc.reload()
      root.nowMs = Date.now()
    }
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
    contentHeight: panel.fittedContentHeight(panel.loadedContentHeight, Style.space(560))

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
                  color: modelData.state === "auth_expired" ? root.urgent : root.foreground
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
                visible: modelData.lastError !== "" && modelData.state !== "auth_expired"
                  && (modelData.state === "error" || modelData.state === "offline")
                text: modelData.lastError
                color: root.urgent
              }
            }
          }

          // ---- Contas ---------------------------------------------------
          PanelSeparator { foreground: root.foreground }
          PanelSectionHeader {
            text: strings.t("accounts")
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          PlainLabel {
            visible: root.running && root.accountsKnown && root.accounts.length === 0
            text: strings.t("noAccounts")
            color: root.dim
          }

          Repeater {
            model: root.running ? root.accounts : []

            Column {
              required property var modelData
              width: column.width
              spacing: Style.space(6)

              Row {
                width: parent.width
                spacing: Style.space(4)
                PlainLabel {
                  width: parent.width - removeButton.width - Style.space(4)
                  text: (modelData.label !== "" ? modelData.label : modelData.name) + "  ·  " + modelData.provider
                    + (modelData.expired ? "  ·  " + strings.t("accountExpired") : "")
                  color: modelData.expired ? root.urgent : root.foreground
                }
                PanelActionButton {
                  id: removeButton
                  iconText: "\u{F0343}"
                  tooltipText: strings.t("disconnect")
                  foreground: root.foreground
                  hoverColor: root.urgent
                  fontFamily: root.fontFamily
                  onClicked: root.confirmRemove = root.confirmRemove === modelData.name ? "" : modelData.name
                }
              }

              Button {
                visible: modelData.expired
                text: strings.t("reauth")
                iconText: "\u{F0306}"
                enabled: !root.authorizing
                foreground: root.foreground
                bordered: true
                horizontalPadding: 8
                verticalPadding: 3
                iconSize: Style.font.bodySmall
                fontSize: Style.font.bodySmall
                onClicked: root.reauth(modelData.name)
              }

              // Confirmação inline antes de desconectar.
              Column {
                visible: root.confirmRemove === modelData.name
                width: parent.width
                spacing: Style.space(6)
                PlainLabel {
                  text: modelData.pairs.length > 0
                    ? strings.t("confirmDisconnectPairs").arg(modelData.label || modelData.name).arg(modelData.pairs.join(", "))
                    : strings.t("confirmDisconnect").arg(modelData.label || modelData.name)
                  color: root.urgent
                }
                Row {
                  spacing: Style.space(6)
                  Button {
                    text: strings.t("confirm")
                    foreground: root.urgent
                    bordered: true
                    horizontalPadding: 8
                    verticalPadding: 3
                    fontSize: Style.font.bodySmall
                    onClicked: root.disconnect(modelData.name)
                  }
                  Button {
                    text: strings.t("cancel")
                    foreground: root.foreground
                    horizontalPadding: 8
                    verticalPadding: 3
                    fontSize: Style.font.bodySmall
                    onClicked: root.confirmRemove = ""
                  }
                }
              }
            }
          }

          // Conta conectada sem nenhuma pasta: um clique cria o par padrão.
          Button {
            visible: root.running && root.pairs.length === 0 && root.firstAccount !== null && !root.firstAccount.expired
            text: root.firstAccount
              ? strings.t("defaultPair").arg(root.firstAccount.provider === "gdrive" ? "~/GoogleDrive" : "~/OneDrive")
              : ""
            iconText: "\u{F04E6}"
            foreground: root.foreground
            bordered: true
            horizontalPadding: 8
            verticalPadding: 4
            iconSize: Style.font.bodySmall
            fontSize: Style.font.bodySmall
            onClicked: root.addDefaultPair(root.firstAccount)
          }

          // Progresso do login.
          Row {
            visible: root.authorizing
            width: parent.width
            spacing: Style.space(8)
            PlainLabel {
              width: parent.width - cancelAuth.width - Style.space(8)
              text: strings.t("authWaiting")
              color: root.foreground
            }
            Button {
              id: cancelAuth
              text: strings.t("authCancel")
              foreground: root.foreground
              bordered: true
              horizontalPadding: 8
              verticalPadding: 3
              fontSize: Style.font.bodySmall
              onClicked: root.run(["account", "cancel"])
            }
          }
          PlainLabel {
            visible: root.authNotice
            text: root.authNoticeText()
            color: root.auth && root.auth.state === "done" ? root.foreground : root.urgent
          }

          Flow {
            width: parent.width
            spacing: Style.space(6)
            Button {
              text: strings.t("connectGoogle")
              iconText: "\u{F02B6}"
              enabled: !root.authorizing
              foreground: root.foreground
              bordered: true
              horizontalPadding: 8
              verticalPadding: 4
              iconSize: Style.font.bodySmall
              fontSize: Style.font.bodySmall
              onClicked: root.connect("gdrive")
            }
            Button {
              text: strings.t("connectMicrosoft")
              iconText: "\u{F03CA}"
              enabled: !root.authorizing
              foreground: root.foreground
              bordered: true
              horizontalPadding: 8
              verticalPadding: 4
              iconSize: Style.font.bodySmall
              fontSize: Style.font.bodySmall
              onClicked: root.connect("onedrive")
            }
          }
        }
      }
    }
  }

  // Todo texto dinâmico é PlainText: nomes de arquivo, e-mails e erros vêm de fora.
  component PlainLabel: Text {
    width: parent ? parent.width : implicitWidth
    textFormat: Text.PlainText
    wrapMode: Text.WordWrap
    font.family: root.fontFamily
    font.pixelSize: Style.font.bodySmall
  }
}

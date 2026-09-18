pragma ComponentBehavior: Bound

import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Hyprland
import Quickshell.Io
import Quickshell.Wayland

ShellRoot {
    id: shell

    readonly property string backendBinary: Quickshell.env("VOICE_INPUT_BIN") || "voice-input"
    readonly property string runtimeDirectory: Quickshell.env("XDG_RUNTIME_DIR")
    property bool panelOpen: false
    property ShellScreen openingScreen: null
    readonly property ShellScreen focusedScreen: {
        const focused = Hyprland.focusedMonitor;
        if (!focused)
            return null;
        // Monitor objects arrive before their names. A binding also observes
        // name changes, unlike a one-time focusedMonitorChanged callback.
        const screens = Quickshell.screens;
        for (let i = 0; i < screens.length; ++i) {
            if (screens[i].name === focused.name)
                return screens[i];
        }
        return null;
    }
    onFocusedScreenChanged: chooseOpeningScreen()
    property var entries: []
    property var selectedEntry: null
    property bool loading: false
    property bool reloadPending: false
    property bool busy: false
    property string loadError: ""
    property string pasteError: ""

    function chooseOpeningScreen() {
        if (panelOpen && !openingScreen && focusedScreen)
            openingScreen = focusedScreen;
    }

    function showPanel() {
        if (panelOpen || busy)
            return;

        openingScreen = null;
        entries = [];
        selectedEntry = null;
        loadError = "";
        pasteError = "";
        panelOpen = true;
        chooseOpeningScreen();
        reloadHistory();
    }

    function hidePanel() {
        // Once confirmed, keep the result visible until delivery succeeds or
        // reports an error. Closing never changes application focus.
        if (busy)
            return;

        panelOpen = false;
        reloadPending = false;
        selectedEntry = null;
        entries = [];
        loadError = "";
        pasteError = "";
    }

    function reloadHistory() {
        if (!panelOpen || busy)
            return;
        if (loading) {
            reloadPending = true;
            return;
        }
        reloadPending = false;
        loadError = "";
        loading = true;
        listProcess.running = true;
    }

    function finishList(success, output, errorText) {
        if (!loading)
            return;

        loading = false;
        if (panelOpen) {
            if (success) {
                try {
                    const result = JSON.parse(output);
                    if (!Array.isArray(result))
                        throw new Error("Invalid history array");
                    for (const entry of result) {
                        if (!entry || !Number.isSafeInteger(entry.id) || !Number.isFinite(entry.completed_at_ms) || typeof entry.text !== "string")
                            throw new Error("Invalid history entry");
                    }
                    const selectedId = selectedEntry ? selectedEntry.id : null;
                    // The backend supplies newest-first ordering. Preserve a
                    // mouse selection when the watched file is replaced.
                    entries = result;
                    selectedEntry = result.find(entry => entry.id === selectedId) || null;
                } catch (error) {
                    loadError = theme.tr("History backend returned invalid data.");
                }
            } else {
                loadError = errorText.trim() || theme.tr("Could not load history.");
            }
        }
        if (reloadPending && panelOpen)
            Qt.callLater(shell.reloadHistory);
    }

    function pasteSelected() {
        if (!panelOpen || busy || loading || loadError.length > 0 || !selectedEntry)
            return;

        pasteError = "";
        busy = true;
        // Only the history ID is sent. The backend pastes into the application
        // focused at confirmation time; this UI never identifies a window.
        pasteProcess.command = [backendBinary, "history", "paste", String(selectedEntry.id)];
        pasteProcess.running = true;
    }

    function finishPaste(success, errorText) {
        if (!busy)
            return;

        busy = false;
        if (success)
            hidePanel();
        else
            pasteError = errorText.trim() || theme.tr("Could not paste transcription.");
    }

    Component.onCompleted: showPanel()

    Theme {
        id: theme
    }

    IpcHandler {
        target: "voiceInputHistory"

        function toggle() {
            if (shell.panelOpen)
                shell.hidePanel();
            else
                shell.showPanel();
        }
    }

    FileView {
        id: historyFile

        path: shell.panelOpen && shell.runtimeDirectory.length > 0 ? shell.runtimeDirectory + "/voice-input/history.json" : ""
        watchChanges: shell.panelOpen
        printErrors: false
        onFileChanged: {
            historyFile.reload();
            shell.reloadHistory();
        }
    }

    Process {
        id: listProcess

        command: [shell.backendBinary, "history", "list"]
        stdout: StdioCollector {
            id: listOutput
            waitForEnd: true
        }
        stderr: StdioCollector {
            id: listErrors
            waitForEnd: true
        }
        onExited: (exitCode, exitStatus) => shell.finishList(exitCode === 0 && exitStatus === 0, listOutput.text, listErrors.text)
        onRunningChanged: {
            if (!listProcess.running) {
                // Let normal exit handlers consume both streams before
                // treating a stopped command without a result as a failure.
                Qt.callLater(() => {
                    if (shell.loading && !listProcess.running)
                        shell.finishList(false, "", theme.tr("Could not start history backend."));
                });
            }
        }
    }

    Process {
        id: pasteProcess

        stdout: StdioCollector {
            id: pasteOutput
            waitForEnd: true
        }
        stderr: StdioCollector {
            id: pasteErrors
            waitForEnd: true
        }
        onExited: (exitCode, exitStatus) => shell.finishPaste(exitCode === 0 && exitStatus === 0 && pasteOutput.text.trim() === "ok", pasteErrors.text)
        onRunningChanged: {
            if (!pasteProcess.running) {
                Qt.callLater(() => {
                    if (shell.busy && !pasteProcess.running)
                        shell.finishPaste(false, theme.tr("Could not start history backend."));
                });
            }
        }
    }

    PanelWindow {
        id: panel

        screen: shell.openingScreen
        visible: shell.panelOpen && shell.openingScreen !== null
        implicitWidth: Math.min(680, shell.openingScreen ? shell.openingScreen.width - 48 : 680)
        implicitHeight: Math.min(460, shell.openingScreen ? shell.openingScreen.height - 48 : 460)
        color: "transparent"
        exclusionMode: ExclusionMode.Ignore
        focusable: false
        WlrLayershell.namespace: "voice-input-history"
        WlrLayershell.layer: WlrLayer.Overlay
        WlrLayershell.keyboardFocus: WlrKeyboardFocus.None

        Rectangle {
            anchors.fill: parent
            color: theme.background
            radius: 6
            border.width: 1
            border.color: theme.border

            ColumnLayout {
                anchors.fill: parent
                anchors.margins: 16
                spacing: 10

                Text {
                    Layout.fillWidth: true
                    text: theme.tr("Transcription history")
                    textFormat: Text.PlainText
                    color: theme.foreground
                    font.pixelSize: 16
                    font.weight: Font.Bold
                }

                Text {
                    Layout.fillWidth: true
                    text: theme.tr("Select an entry, then paste into the focused application.")
                    textFormat: Text.PlainText
                    color: theme.subtle
                    font.pixelSize: 12
                    wrapMode: Text.WordWrap
                }

                RowLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 12

                    Rectangle {
                        Layout.preferredWidth: 240
                        Layout.fillHeight: true
                        color: theme.surface
                        radius: 4
                        border.width: 1
                        border.color: theme.border

                        ListView {
                            id: historyList

                            anchors.fill: parent
                            anchors.margins: 5
                            model: shell.entries
                            spacing: 4
                            clip: true
                            focus: false
                            activeFocusOnTab: false
                            keyNavigationEnabled: false
                            boundsBehavior: Flickable.StopAtBounds

                            ScrollBar.vertical: ScrollBar {
                                policy: ScrollBar.AsNeeded
                                focusPolicy: Qt.NoFocus
                                activeFocusOnTab: false
                            }

                            delegate: Rectangle {
                                id: entryRow

                                required property var modelData
                                readonly property bool selected: shell.selectedEntry !== null && shell.selectedEntry.id === modelData.id
                                width: historyList.width - 10
                                height: 70
                                radius: 3
                                color: selected ? theme.elevated : (rowMouse.containsMouse ? Qt.alpha(theme.foreground, 0.06) : "transparent")
                                border.width: selected ? 1 : 0
                                border.color: theme.accent

                                Column {
                                    anchors.fill: parent
                                    anchors.margins: 8
                                    spacing: 5

                                    Text {
                                        width: parent.width
                                        text: new Date(entryRow.modelData.completed_at_ms).toLocaleString(Qt.locale(theme.i18n.locale), Locale.ShortFormat)
                                        textFormat: Text.PlainText
                                        color: theme.subtle
                                        font.pixelSize: 10
                                        elide: Text.ElideRight
                                    }

                                    Text {
                                        width: parent.width
                                        text: entryRow.modelData.text.replace(/\s+/g, " ")
                                        textFormat: Text.PlainText
                                        color: theme.foreground
                                        font.pixelSize: 12
                                        wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                                        maximumLineCount: 2
                                        elide: Text.ElideRight
                                    }
                                }

                                MouseArea {
                                    id: rowMouse

                                    anchors.fill: parent
                                    enabled: !shell.busy && !shell.loading
                                    acceptedButtons: Qt.LeftButton
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        shell.selectedEntry = entryRow.modelData;
                                        shell.pasteError = "";
                                        previewScroll.contentY = 0;
                                    }
                                }
                            }
                        }

                        Text {
                            anchors.centerIn: parent
                            width: parent.width - 24
                            visible: shell.entries.length === 0
                            text: shell.loading ? theme.tr("Loading history…") : (shell.loadError.length > 0 ? theme.tr("Could not load history.") : theme.tr("No transcriptions yet."))
                            textFormat: Text.PlainText
                            color: theme.subtle
                            font.pixelSize: 12
                            horizontalAlignment: Text.AlignHCenter
                            wrapMode: Text.WordWrap
                        }
                    }

                    ColumnLayout {
                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        spacing: 8

                        Text {
                            text: theme.tr("Preview")
                            textFormat: Text.PlainText
                            color: theme.subtle
                            font.pixelSize: 12
                            font.weight: Font.Bold
                        }

                        Flickable {
                            id: previewScroll

                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            contentWidth: width
                            contentHeight: previewText.implicitHeight
                            clip: true
                            focus: false
                            activeFocusOnTab: false
                            flickableDirection: Flickable.VerticalFlick
                            boundsBehavior: Flickable.StopAtBounds

                            ScrollBar.vertical: ScrollBar {
                                policy: ScrollBar.AsNeeded
                                focusPolicy: Qt.NoFocus
                                activeFocusOnTab: false
                            }

                            Text {
                                id: previewText

                                width: previewScroll.width - 12
                                text: shell.selectedEntry ? shell.selectedEntry.text : theme.tr("Select an entry to preview it.")
                                textFormat: Text.PlainText
                                color: shell.selectedEntry ? theme.foreground : theme.subtle
                                font.pixelSize: 13
                                wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                                lineHeight: 1.25
                            }
                        }
                    }
                }

                Text {
                    Layout.fillWidth: true
                    visible: text.length > 0
                    text: shell.pasteError || shell.loadError
                    textFormat: Text.PlainText
                    color: theme.error
                    font.pixelSize: 12
                    wrapMode: Text.WrapAtWordBoundaryOrAnywhere
                    maximumLineCount: 3
                    elide: Text.ElideRight
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 8

                    Text {
                        Layout.fillWidth: true
                        text: shell.loading && shell.entries.length > 0 ? theme.tr("Loading history…") : ""
                        textFormat: Text.PlainText
                        color: theme.subtle
                        font.pixelSize: 11
                    }

                    AppButton {
                        theme: theme
                        text: "Close"
                        focusPolicy: Qt.NoFocus
                        activeFocusOnTab: false
                        enabled: !shell.busy
                        opacity: enabled ? 1 : 0.5
                        onClicked: shell.hidePanel()
                    }

                    AppButton {
                        theme: theme
                        text: shell.busy ? "Pasting…" : "Paste"
                        primary: true
                        focusPolicy: Qt.NoFocus
                        activeFocusOnTab: false
                        enabled: shell.selectedEntry !== null && !shell.loading && !shell.busy && shell.loadError.length === 0
                        opacity: enabled ? 1 : 0.5
                        onClicked: shell.pasteSelected()
                    }
                }
            }
        }
    }
}

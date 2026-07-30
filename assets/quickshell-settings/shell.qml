import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import Quickshell
import Quickshell.Io

ShellRoot {
    id: shell

    readonly property var destinations: [{
        "title": "Overview",
        "glyph": "⌂"
    }, {
        "title": "Speech",
        "glyph": "◉"
    }, {
        "title": "Refinement",
        "glyph": "✦"
    }, {
        "title": "Output",
        "glyph": "→"
    }, {
        "title": "Appearance",
        "glyph": "◇"
    }, {
        "title": "Advanced",
        "glyph": "⚙"
    }]

    function destinationIndex(title) {
        for (let i = 0; i < destinations.length; ++i) {
            if (destinations[i].title === title)
                return i;

        }
        return 0;
    }

    function navigate(title) {
        destinationList.currentIndex = destinationIndex(title);
        contentScroll.contentItem.contentY = 0;
    }

    function statusService() {
        const status = controller.runtimeStatus || {
        };
        return status.service && typeof status.service === "object" ? status.service : {
        };
    }

    function statusRuntime() {
        const status = controller.runtimeStatus || {
        };
        return status.runtime && typeof status.runtime === "object" ? status.runtime : {
        };
    }

    function serviceRunning() {
        const service = statusService();
        if (service.running === true || service.running === false)
            return service.running;

        const state = service.active_state !== undefined ? service.active_state : service.state;
        if (state === "active" || state === "running")
            return true;

        if (state === "inactive" || state === "failed" || state === "stopped")
            return false;

        return undefined;
    }

    function serviceSummary() {
        if (controller.runtimeLoading && controller.runtimeStatusState === "unknown")
            return theme.tr("Checking local service status…");

        if (controller.runtimeStatusState !== "available")
            return theme.tr(controller.runtimeStatusMessage || "Runtime status is unavailable.");

        const running = serviceRunning();
        if (running === true)
            return theme.tr("Running");

        if (running === false)
            return theme.tr("Not running");

        const service = statusService();
        const state = service.active_state !== undefined ? service.active_state : service.state;
        if (state !== undefined && String(state).length > 0)
            return theme.tr("Service state: " + String(state));

        return theme.tr("Service state was not reported.");
    }

    function runtimeSummary() {
        if (controller.runtimeStatusState !== "available")
            return theme.tr("No runtime details reported.");

        const runtime = statusRuntime();
        if (runtime.available === false)
            return theme.tr("Runtime details are unavailable.");

        const parts = [];
        if (runtime.phase !== undefined && String(runtime.phase).length > 0)
            parts.push(theme.tr("Phase: " + String(runtime.phase)));

        if (runtime.language !== undefined && String(runtime.language).length > 0)
            parts.push(theme.tr("Language: " + String(runtime.language)));

        const updated = Number(runtime.updated_at_ms);
        if (Number.isFinite(updated) && updated > 0)
            parts.push(theme.tr("Updated: " + new Date(updated).toLocaleString(Qt.locale(theme.i18n.locale))));

        return parts.length > 0 ? parts.join(" · ") : theme.tr("No runtime details reported.");
    }

    function serviceUnit() {
        const service = statusService();
        return service.unit !== undefined && String(service.unit).length > 0 ? String(service.unit) : theme.tr("Voice Input service");
    }

    Theme {
        id: theme
    }

    SettingsController {
        id: controller
    }

    IpcHandler {
        function activate() {
            window.visible = true;
            window.raise();
            window.requestActivate();
        }

        target: "voiceInputSettings"
    }

    Connections {
        function revealAdvancedErrors() {
            if (controller.pageHasAdvancedError("Speech"))
                speechPage.advancedExpanded = true;

            if (controller.pageHasAdvancedError("Refinement"))
                refinementPage.advancedExpanded = true;

            if (controller.pageHasAdvancedError("Output"))
                outputPage.advancedExpanded = true;

            if (controller.pageHasAdvancedError("Appearance"))
                appearancePage.advancedExpanded = true;

        }

        function onFieldErrorsChanged() {
            revealAdvancedErrors();
        }

        function onRouteRequested(page, advanced) {
            revealAdvancedErrors();
            shell.navigate(page);
            if (!advanced)
                return ;

            if (page === "Speech")
                speechPage.advancedExpanded = true;
            else if (page === "Refinement")
                refinementPage.advancedExpanded = true;
            else if (page === "Output")
                outputPage.advancedExpanded = true;
            else if (page === "Appearance")
                appearancePage.advancedExpanded = true;
        }

        target: controller
    }

    FloatingWindow {
        id: window

        function raise() {
            if (contentItem.window)
                contentItem.window.raise();

        }

        function requestActivate() {
            if (contentItem.window)
                contentItem.window.requestActivate();

        }

        function requestClose() {
            if (controller.busy)
                return ;

            if (controller.dirty)
                closeDialog.open();
            else
                Qt.quit();
        }

        title: controller.dirty ? "Voice Input Settings •" : "Voice Input Settings"
        visible: true
        implicitWidth: 900
        implicitHeight: 620
        minimumSize: Qt.size(720, 540)
        color: theme.background
        onClosed: Qt.quit()

        Connections {
            function onClosing(close) {
                if (controller.busy) {
                    close.accepted = false;
                } else if (controller.dirty) {
                    close.accepted = false;
                    closeDialog.open();
                }
            }

            ignoreUnknownSignals: true
            target: window.contentItem && window.contentItem.window ? window.contentItem.window : null
        }

        Rectangle {
            anchors.fill: parent
            color: theme.background

            RowLayout {
                anchors.fill: parent
                spacing: 0

                Rectangle {
                    Layout.preferredWidth: 176
                    Layout.minimumWidth: 176
                    Layout.maximumWidth: 176
                    Layout.fillHeight: true
                    color: Qt.darker(theme.surface, 1.06)
                    border.color: theme.border

                    ColumnLayout {
                        anchors.fill: parent
                        anchors.margins: 10
                        spacing: 8

                        RowLayout {
                            Layout.fillWidth: true
                            Layout.preferredHeight: 46
                            spacing: 9

                            Rectangle {
                                Layout.preferredWidth: 30
                                Layout.preferredHeight: 30
                                radius: 8
                                color: Qt.alpha(theme.accent, 0.16)
                                border.color: Qt.alpha(theme.accent, 0.32)

                                Text {
                                    anchors.centerIn: parent
                                    text: "VI"
                                    color: theme.accent
                                    font.pixelSize: 11
                                    font.bold: true
                                }

                            }

                            Label {
                                text: theme.tr("Voice Input")
                                color: theme.foreground
                                font.pixelSize: 14
                                font.weight: Font.DemiBold
                                Layout.fillWidth: true
                            }

                        }

                        ListView {
                            id: destinationList

                            Layout.fillWidth: true
                            Layout.fillHeight: true
                            spacing: 3
                            clip: true
                            model: shell.destinations
                            currentIndex: 0
                            activeFocusOnTab: true
                            Accessible.name: theme.tr("Settings destinations")
                            keyNavigationWraps: true

                            delegate: ItemDelegate {
                                required property var modelData
                                required property int index
                                readonly property int errorCount: controller.errorCountForPage(modelData.title)

                                width: destinationList.width
                                height: 44
                                highlighted: destinationList.currentIndex === index
                                leftPadding: 12
                                rightPadding: 9
                                Accessible.name: theme.tr(modelData.title)
                                onClicked: shell.navigate(modelData.title)

                                Rectangle {
                                    visible: parent.highlighted
                                    anchors.left: parent.left
                                    anchors.top: parent.top
                                    anchors.bottom: parent.bottom
                                    anchors.topMargin: 8
                                    anchors.bottomMargin: 8
                                    width: 3
                                    radius: 2
                                    color: theme.accent
                                }

                                contentItem: RowLayout {
                                    spacing: 9

                                    Text {
                                        Layout.preferredWidth: 18
                                        text: modelData.glyph
                                        color: destinationList.currentIndex === index ? theme.accent : theme.subtle
                                        font.pixelSize: 14
                                        horizontalAlignment: Text.AlignHCenter
                                    }

                                    Text {
                                        text: theme.tr(modelData.title)
                                        color: theme.foreground
                                        font.pixelSize: 12
                                        font.weight: destinationList.currentIndex === index ? Font.DemiBold : Font.Medium
                                        elide: Text.ElideRight
                                        Layout.fillWidth: true
                                    }

                                    Rectangle {
                                        visible: errorCount > 0
                                        Layout.preferredWidth: Math.max(18, errorNumber.implicitWidth + 8)
                                        Layout.preferredHeight: 18
                                        radius: 9
                                        color: theme.error

                                        Label {
                                            id: errorNumber

                                            anchors.centerIn: parent
                                            text: String(errorCount)
                                            color: theme.background
                                            font.pixelSize: 9
                                            font.bold: true
                                        }

                                    }

                                }

                                background: Rectangle {
                                    radius: 7
                                    color: parent.highlighted ? Qt.alpha(theme.accent, 0.1) : (parent.hovered ? theme.elevated : "transparent")
                                }

                            }

                        }

                        Rectangle {
                            Layout.fillWidth: true
                            Layout.preferredHeight: 72
                            radius: 9
                            color: Qt.alpha(theme.elevated, 0.64)
                            border.color: theme.border

                            ColumnLayout {
                                anchors.fill: parent
                                anchors.margins: 10
                                spacing: 4

                                RowLayout {
                                    Layout.fillWidth: true
                                    spacing: 7

                                    Rectangle {
                                        Layout.preferredWidth: 8
                                        Layout.preferredHeight: 8
                                        radius: 4
                                        color: shell.serviceRunning() === true ? theme.success : (shell.serviceRunning() === false ? theme.error : theme.muted)
                                    }

                                    Label {
                                        text: theme.tr("Local service")
                                        color: theme.foreground
                                        font.pixelSize: 11
                                        font.weight: Font.DemiBold
                                        Layout.fillWidth: true
                                    }

                                }

                                Label {
                                    text: shell.serviceSummary()
                                    color: theme.subtle
                                    font.pixelSize: 9
                                    elide: Text.ElideRight
                                    Layout.fillWidth: true
                                }

                            }

                        }

                    }

                }

                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 0

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 54
                        color: theme.surface
                        border.color: theme.border

                        RowLayout {
                            anchors.fill: parent
                            anchors.leftMargin: 16
                            anchors.rightMargin: 10
                            spacing: 8

                            Label {
                                text: theme.tr(shell.destinations[Math.max(0, destinationList.currentIndex)].title)
                                color: theme.foreground
                                font.pixelSize: 16
                                font.weight: Font.DemiBold
                                Layout.fillWidth: true
                            }

                            Label {
                                visible: controller.dirty
                                text: theme.tr("Unsaved")
                                color: theme.warning
                                font.pixelSize: 10
                            }

                            Rectangle {
                                Layout.preferredWidth: 88
                                Layout.preferredHeight: 30
                                radius: 7
                                color: theme.elevated
                                border.color: theme.border

                                RowLayout {
                                    anchors.fill: parent
                                    anchors.margins: 2
                                    spacing: 2

                                    Button {
                                        Layout.fillWidth: true
                                        Layout.fillHeight: true
                                        text: "EN"
                                        flat: true
                                        Accessible.name: theme.tr("Switch settings language to English")
                                        onClicked: theme.i18n.setLocale("en")

                                        contentItem: Text {
                                            text: parent.text
                                            color: theme.i18n.locale === "en" ? theme.background : theme.subtle
                                            font.pixelSize: 10
                                            font.weight: Font.DemiBold
                                            horizontalAlignment: Text.AlignHCenter
                                            verticalAlignment: Text.AlignVCenter
                                        }

                                        background: Rectangle {
                                            radius: 5
                                            color: theme.i18n.locale === "en" ? theme.accent : (parent.hovered ? Qt.alpha(theme.accent, 0.12) : "transparent")
                                            border.width: parent.activeFocus ? 1 : 0
                                            border.color: theme.accent
                                        }

                                    }

                                    Label {
                                        text: "/"
                                        color: theme.subtle
                                        font.pixelSize: 9
                                    }

                                    Button {
                                        Layout.fillWidth: true
                                        Layout.fillHeight: true
                                        text: "中文"
                                        flat: true
                                        Accessible.name: theme.tr("Switch settings language to Simplified Chinese")
                                        onClicked: theme.i18n.setLocale("zh-CN")

                                        contentItem: Text {
                                            text: parent.text
                                            color: theme.i18n.locale === "zh-CN" ? theme.background : theme.subtle
                                            font.pixelSize: 10
                                            font.weight: Font.DemiBold
                                            horizontalAlignment: Text.AlignHCenter
                                            verticalAlignment: Text.AlignVCenter
                                        }

                                        background: Rectangle {
                                            radius: 5
                                            color: theme.i18n.locale === "zh-CN" ? theme.accent : (parent.hovered ? Qt.alpha(theme.accent, 0.12) : "transparent")
                                            border.width: parent.activeFocus ? 1 : 0
                                            border.color: theme.accent
                                        }

                                    }

                                }

                            }

                            ToolButton {
                                id: overflowButton

                                text: "⋮"
                                Accessible.name: theme.tr("More actions")
                                onClicked: overflowMenu.open()

                                Menu {
                                    id: overflowMenu

                                    y: overflowButton.height

                                    MenuItem {
                                        text: theme.tr(controller.loading ? "Reloading…" : "Reload settings")
                                        enabled: !controller.busy
                                        onTriggered: controller.reload()
                                    }

                                }

                                contentItem: Text {
                                    text: parent.text
                                    color: theme.foreground
                                    font.pixelSize: 19
                                    horizontalAlignment: Text.AlignHCenter
                                    verticalAlignment: Text.AlignVCenter
                                }

                                background: Rectangle {
                                    color: parent.hovered ? theme.elevated : "transparent"
                                    radius: 7
                                }

                            }

                            AppButton {
                                theme: theme
                                text: "Close"
                                enabled: !controller.busy
                                onClicked: window.requestClose()
                            }

                        }

                    }

                    ScrollView {
                        id: contentScroll

                        Layout.fillWidth: true
                        Layout.fillHeight: true
                        clip: true
                        contentWidth: availableWidth
                        ScrollBar.horizontal.policy: ScrollBar.AlwaysOff

                        StackLayout {
                            id: pageStack

                            width: contentScroll.availableWidth
                            height: children[currentIndex] ? children[currentIndex].implicitHeight : 0
                            currentIndex: destinationList.currentIndex

                            OverviewPage {
                                id: overviewPage

                                theme: theme
                                controller: controller
                                serviceRunning: shell.serviceRunning()
                                serviceSummary: shell.serviceSummary()
                                runtimeSummary: shell.runtimeSummary()
                                serviceUnit: shell.serviceUnit()
                                onNavigateRequested: (page) => {
                                    return shell.navigate(page);
                                }
                            }

                            SpeechPage {
                                id: speechPage

                                theme: theme
                                controller: controller
                            }

                            RefinementPage {
                                id: refinementPage

                                theme: theme
                                controller: controller
                            }

                            OutputPage {
                                id: outputPage

                                theme: theme
                                controller: controller
                            }

                            AppearancePage {
                                id: appearancePage

                                theme: theme
                                controller: controller
                            }

                            AdvancedPage {
                                id: advancedPage

                                theme: theme
                                controller: controller
                            }

                        }

                    }

                    SettingsFooter {
                        theme: theme
                        controller: controller
                    }

                }

            }

        }

        CloseConfirmationDialog {
            id: closeDialog

            parent: window.contentItem
            theme: theme
            controller: controller
        }

    }

}

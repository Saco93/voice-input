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

    function providerLabel(value) {
        return theme.tr(value === "alibaba-qwen-realtime" ? "Alibaba Qwen realtime" : "Local CLI");
    }

    function outputLabel(value) {
        if (value === "clipboard")
            return theme.tr("Clipboard only");

        if (value === "paste")
            return theme.tr("Clipboard paste");

        return theme.tr("Direct typing");
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
                speechAdvanced.expanded = true;

            if (controller.pageHasAdvancedError("Refinement"))
                refinementAdvanced.expanded = true;

            if (controller.pageHasAdvancedError("Output"))
                outputAdvanced.expanded = true;

            if (controller.pageHasAdvancedError("Appearance"))
                appearanceAdvanced.expanded = true;

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
                speechAdvanced.expanded = true;
            else if (page === "Refinement")
                refinementAdvanced.expanded = true;
            else if (page === "Output")
                outputAdvanced.expanded = true;
            else if (page === "Appearance")
                appearanceAdvanced.expanded = true;
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

                            SettingsPage {
                                id: overviewPage

                                theme: theme
                                title: "Overview"
                                description: "See the local service report and review the settings you are preparing to use."

                                Rectangle {
                                    Layout.fillWidth: true
                                    implicitHeight: 64
                                    radius: 10
                                    color: theme.surface
                                    border.color: theme.border

                                    RowLayout {
                                        anchors.fill: parent
                                        anchors.margins: 12
                                        spacing: 10

                                        Rectangle {
                                            Layout.preferredWidth: 10
                                            Layout.preferredHeight: 10
                                            radius: 5
                                            color: shell.serviceRunning() === true ? theme.success : (shell.serviceRunning() === false ? theme.error : theme.muted)
                                        }

                                        ColumnLayout {
                                            Layout.fillWidth: true
                                            spacing: 2

                                            Label {
                                                text: shell.serviceUnit() + " — " + shell.serviceSummary()
                                                color: theme.foreground
                                                font.pixelSize: 12
                                                font.weight: Font.DemiBold
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }

                                            Label {
                                                text: shell.runtimeSummary()
                                                color: theme.subtle
                                                font.pixelSize: 10
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }

                                        }

                                        Label {
                                            text: theme.tr(controller.runtimeLoading ? "Refreshing…" : "Local report")
                                            color: theme.subtle
                                            font.pixelSize: 9
                                        }

                                    }

                                }

                                Rectangle {
                                    Layout.fillWidth: true
                                    implicitHeight: 66
                                    radius: 10
                                    color: Qt.alpha(theme.surface, 0.72)
                                    border.color: theme.border

                                    RowLayout {
                                        anchors.fill: parent
                                        anchors.margins: 12
                                        spacing: 8

                                        ColumnLayout {
                                            Layout.fillWidth: true
                                            spacing: 2

                                            Label {
                                                text: theme.tr("Speech")
                                                color: theme.foreground
                                                font.pixelSize: 11
                                                font.weight: Font.DemiBold
                                            }

                                            Label {
                                                text: shell.providerLabel(controller.value("asr.provider", "local-cli"))
                                                color: theme.subtle
                                                font.pixelSize: 9
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }

                                        }

                                        Label {
                                            text: "→"
                                            color: theme.accent
                                            font.pixelSize: 14
                                        }

                                        ColumnLayout {
                                            Layout.fillWidth: true
                                            spacing: 2

                                            Label {
                                                text: theme.tr("Refinement")
                                                color: theme.foreground
                                                font.pixelSize: 11
                                                font.weight: Font.DemiBold
                                            }

                                            Label {
                                                text: theme.tr(controller.value("llm.enabled", false) ? "Enabled" : "Skipped")
                                                color: theme.subtle
                                                font.pixelSize: 9
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }

                                        }

                                        Label {
                                            text: "→"
                                            color: theme.accent
                                            font.pixelSize: 14
                                        }

                                        ColumnLayout {
                                            Layout.fillWidth: true
                                            spacing: 2

                                            Label {
                                                text: theme.tr("Output")
                                                color: theme.foreground
                                                font.pixelSize: 11
                                                font.weight: Font.DemiBold
                                            }

                                            Label {
                                                text: shell.outputLabel(controller.value("output.mode", "type"))
                                                color: theme.subtle
                                                font.pixelSize: 9
                                                elide: Text.ElideRight
                                                Layout.fillWidth: true
                                            }

                                        }

                                    }

                                }

                                SettingsGrid {
                                    collapseWidth: 460

                                    SummaryCard {
                                        theme: theme
                                        title: "Speech"
                                        pending: controller.dirty
                                        summary: shell.providerLabel(controller.value("asr.provider", "local-cli"))
                                        detail: "Language: " + controller.value("asr.language", "simplified-chinese")
                                        onActivated: shell.navigate("Speech")
                                    }

                                    SummaryCard {
                                        theme: theme
                                        title: "Refinement"
                                        pending: controller.dirty
                                        summary: controller.value("llm.enabled", false) ? "Enabled" : "Disabled"
                                        detail: controller.value("llm.model", "").length > 0 ? "Model: " + controller.value("llm.model", "") : "Model: provider default"
                                        onActivated: shell.navigate("Refinement")
                                    }

                                    SummaryCard {
                                        theme: theme
                                        title: "Output"
                                        pending: controller.dirty
                                        summary: shell.outputLabel(controller.value("output.mode", "type"))
                                        detail: controller.value("ime.manage_fcitx5", true) ? "Coordinates with Fcitx5" : "Leaves Fcitx5 unchanged"
                                        onActivated: shell.navigate("Output")
                                    }

                                    SummaryCard {
                                        theme: theme
                                        title: "Appearance"
                                        pending: controller.dirty
                                        summary: controller.value("hud.enabled", true) ? "HUD visible" : "HUD hidden"
                                        detail: "Position: " + String(controller.value("hud.position", "bottom-center")).replace(/-/g, " ")
                                        onActivated: shell.navigate("Appearance")
                                    }

                                }

                                Label {
                                    visible: controller.dirty
                                    text: theme.tr("These summaries include unsaved changes. Save to apply them to Voice Input.")
                                    color: theme.warning
                                    font.pixelSize: 10
                                    wrapMode: Text.WordWrap
                                    Layout.fillWidth: true
                                }

                            }

                            SettingsPage {
                                id: speechPage

                                theme: theme
                                title: "Speech"
                                description: "Configure audio capture and speech recognition."

                                SettingsGrid {
                                    id: speechGrid

                                    SectionCard {
                                        theme: theme
                                        title: "Audio capture"
                                        description: "Recording source and capture window."

                                        SettingTextField {
                                            theme: theme
                                            label: "Device"
                                            value: controller.value("audio.device", "default")
                                            error: controller.errorFor("audio.device")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("audio.device", value);
                                            }
                                        }

                                        SettingTextField {
                                            theme: theme
                                            label: "Maximum duration"
                                            value: controller.value("audio.max_duration_secs", 90)
                                            help: "Seconds."
                                            error: controller.errorFor("audio.max_duration_secs")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("audio.max_duration_secs", value);
                                            }
                                        }

                                        SettingSwitch {
                                            theme: theme
                                            label: "Enable pre-roll"
                                            checked: controller.value("audio.pre_roll_enabled", false)
                                            help: "Keep a short capture buffer so the first syllable is preserved."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("audio.pre_roll_enabled", checked);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "Recognition"
                                        description: "Recognition provider, language, and fallback."

                                        SettingCombo {
                                            theme: theme
                                            label: "Provider"
                                            value: controller.value("asr.provider", "local-cli")
                                            labels: ["Local CLI", "Alibaba Qwen realtime"]
                                            values: ["local-cli", "alibaba-qwen-realtime"]
                                            error: controller.errorFor("asr.provider")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("asr.provider", value);
                                            }
                                        }

                                        SettingCombo {
                                            theme: theme
                                            label: "Language"
                                            value: controller.value("asr.language", "simplified-chinese")
                                            labels: ["English", "Simplified Chinese", "Traditional Chinese", "Japanese", "Korean"]
                                            values: ["english", "simplified-chinese", "traditional-chinese", "japanese", "korean"]
                                            error: controller.errorFor("asr.language")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("asr.language", value);
                                            }
                                        }

                                        SettingSwitch {
                                            theme: theme
                                            label: "Fallback to local"
                                            checked: controller.value("asr.fallback_to_local", true)
                                            help: "Use local recognition when remote recognition fails."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("asr.fallback_to_local", checked);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        visible: controller.value("asr.provider", "local-cli") === "local-cli" || controller.value("asr.fallback_to_local", true) || controller.hasErrorPrefix("asr.backend_command") || controller.hasErrorPrefix("asr.engine") || controller.hasErrorPrefix("asr.model")
                                        theme: theme
                                        title: "Local recognition"
                                        description: "Local CLI backend used as the provider or fallback."

                                        SettingTextField {
                                            theme: theme
                                            label: "Backend command"
                                            value: controller.value("asr.backend_command", "")
                                            help: "Executable used by local recognition."
                                            error: controller.errorFor("asr.backend_command")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("asr.backend_command", value);
                                            }
                                        }

                                        SettingTextField {
                                            theme: theme
                                            label: "Local engine"
                                            value: controller.value("asr.engine", "")
                                            error: controller.errorFor("asr.engine")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("asr.engine", value);
                                            }
                                        }

                                        SettingTextField {
                                            theme: theme
                                            label: "Local model"
                                            value: controller.value("asr.model", "")
                                            placeholderText: "Backend default"
                                            error: controller.errorFor("asr.model")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("asr.model", value);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        visible: controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || controller.hasErrorPrefix("asr.alibaba.") || controller.errorFor("credentials.alibaba-api-key").length > 0
                                        theme: theme
                                        title: "Alibaba realtime"
                                        description: "Credential and realtime recognition behavior."

                                        SettingTextField {
                                            theme: theme
                                            label: "Replace Alibaba API key"
                                            value: controller.alibabaCredential
                                            help: controller.credentialLabel("alibaba-api-key") + ". Blank keeps it unchanged."
                                            password: true
                                            placeholderText: "Enter a new credential"
                                            error: controller.errorFor("credentials.alibaba-api-key")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                controller.alibabaCredential = value;
                                                controller.clearFieldError("credentials.alibaba-api-key");
                                                controller.statusMessage = "";
                                            }
                                        }

                                        SettingTextField {
                                            theme: theme
                                            label: "Realtime model"
                                            value: controller.value("asr.alibaba.model", "")
                                            error: controller.errorFor("asr.alibaba.model")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("asr.alibaba.model", value);
                                            }
                                        }

                                        SettingCombo {
                                            theme: theme
                                            label: "Turn mode"
                                            value: controller.value("asr.alibaba.turn_mode", "server-vad")
                                            labels: ["Server VAD", "Manual commit"]
                                            values: ["server-vad", "manual"]
                                            error: controller.errorFor("asr.alibaba.turn_mode")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("asr.alibaba.turn_mode", value);
                                            }
                                        }

                                    }

                                }

                                AdvancedSection {
                                    id: speechAdvanced

                                    theme: theme
                                    description: "Capture cadence, timeouts, and Alibaba tuning."

                                    SettingsGrid {
                                        SectionCard {
                                            theme: theme
                                            title: "Audio tuning"

                                            SettingTextField {
                                                theme: theme
                                                label: "Sample rate"
                                                value: controller.value("audio.sample_rate", 16000)
                                                help: "Samples per second (Hz)."
                                                error: controller.errorFor("audio.sample_rate")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("audio.sample_rate", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Partial interval"
                                                value: controller.value("audio.partial_interval_ms", 1500)
                                                help: "Milliseconds between partial updates."
                                                error: controller.errorFor("audio.partial_interval_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("audio.partial_interval_ms", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Pre-roll window"
                                                value: controller.value("audio.pre_roll_ms", 500)
                                                help: "Milliseconds retained before activation."
                                                error: controller.errorFor("audio.pre_roll_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("audio.pre_roll_ms", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            theme: theme
                                            title: "Recognition timeouts"

                                            SettingTextField {
                                                theme: theme
                                                label: "Connect timeout"
                                                value: controller.value("asr.connect_timeout_ms", 5000)
                                                help: "Milliseconds."
                                                error: controller.errorFor("asr.connect_timeout_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.connect_timeout_ms", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Finalize timeout"
                                                value: controller.value("asr.finalize_timeout_ms", 8000)
                                                help: "Milliseconds."
                                                error: controller.errorFor("asr.finalize_timeout_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.finalize_timeout_ms", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            visible: controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || controller.hasErrorPrefix("asr.alibaba.")
                                            theme: theme
                                            title: "Alibaba tuning"

                                            SettingTextField {
                                                theme: theme
                                                label: "Endpoint"
                                                value: controller.value("asr.alibaba.endpoint", "")
                                                error: controller.errorFor("asr.alibaba.endpoint")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.endpoint", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "VAD threshold"
                                                value: controller.value("asr.alibaba.vad_threshold", 0.2)
                                                error: controller.errorFor("asr.alibaba.vad_threshold")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.vad_threshold", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Silence duration"
                                                value: controller.value("asr.alibaba.silence_duration_ms", 400)
                                                help: "Milliseconds."
                                                error: controller.errorFor("asr.alibaba.silence_duration_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.silence_duration_ms", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            visible: controller.value("asr.provider", "local-cli") === "alibaba-qwen-realtime" || controller.hasErrorPrefix("asr.alibaba.final_pass_")
                                            theme: theme
                                            title: "Alibaba final pass"

                                            SettingSwitch {
                                                theme: theme
                                                label: "Enable final pass"
                                                checked: controller.value("asr.alibaba.final_pass_enabled", false)
                                                enabled: !controller.busy
                                                onToggled: (checked) => {
                                                    return controller.setValue("asr.alibaba.final_pass_enabled", checked);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Base URL"
                                                value: controller.value("asr.alibaba.final_pass_base_url", "")
                                                error: controller.errorFor("asr.alibaba.final_pass_base_url")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.final_pass_base_url", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Model"
                                                value: controller.value("asr.alibaba.final_pass_model", "")
                                                error: controller.errorFor("asr.alibaba.final_pass_model")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.final_pass_model", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Timeout"
                                                value: controller.value("asr.alibaba.final_pass_timeout_ms", 20000)
                                                help: "Milliseconds."
                                                error: controller.errorFor("asr.alibaba.final_pass_timeout_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("asr.alibaba.final_pass_timeout_ms", value);
                                                }
                                            }

                                            SettingSwitch {
                                                theme: theme
                                                label: "Enable ITN"
                                                checked: controller.value("asr.alibaba.final_pass_enable_itn", false)
                                                help: "Apply inverse text normalization."
                                                enabled: !controller.busy
                                                onToggled: (checked) => {
                                                    return controller.setValue("asr.alibaba.final_pass_enable_itn", checked);
                                                }
                                            }

                                        }

                                    }

                                }

                            }

                            SettingsPage {
                                id: refinementPage

                                theme: theme
                                title: "Refinement"
                                description: "Optionally refine recognized transcripts with an LLM."

                                SettingsGrid {
                                    SectionCard {
                                        theme: theme
                                        title: "Refinement"

                                        SettingSwitch {
                                            theme: theme
                                            label: "Enable refinement"
                                            checked: controller.value("llm.enabled", false)
                                            help: "Conservatively refine recognized transcripts."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("llm.enabled", checked);
                                            }
                                        }

                                        SettingTextField {
                                            theme: theme
                                            label: "Model"
                                            value: controller.value("llm.model", "")
                                            placeholderText: "Provider default"
                                            error: controller.errorFor("llm.model")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("llm.model", value);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "Credential"
                                        description: "The replacement is sent only to the backend and is never copied into the draft."

                                        SettingTextField {
                                            theme: theme
                                            label: "Replace OpenRouter API key"
                                            value: controller.openrouterCredential
                                            help: controller.credentialLabel("openrouter-api-key") + ". Blank uses the stored credential."
                                            password: true
                                            placeholderText: "Enter a new credential"
                                            error: controller.errorFor("credentials.openrouter-api-key")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                controller.openrouterCredential = value;
                                                controller.clearFieldError("credentials.openrouter-api-key");
                                                controller.statusMessage = "";
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "Context"

                                        SettingSwitch {
                                            theme: theme
                                            label: "Use agent context"
                                            checked: controller.value("llm.agent_context_enabled", false)
                                            help: "Send a redacted excerpt from the focused Pi or Codex session."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("llm.agent_context_enabled", checked);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "Test refinement"
                                        description: "Test the current LLM draft and credential without saving it."

                                        AppButton {
                                            theme: theme
                                            text: controller.testing ? "Testing…" : "Test LLM"
                                            enabled: !controller.busy
                                            onClicked: controller.testLlm()
                                        }

                                    }

                                }

                                AdvancedSection {
                                    id: refinementAdvanced

                                    theme: theme
                                    description: "Endpoint, timeout, provider ordering, and context limits."

                                    SettingsGrid {
                                        SectionCard {
                                            theme: theme
                                            title: "Provider settings"

                                            SettingTextField {
                                                theme: theme
                                                label: "API base URL"
                                                value: controller.value("llm.api_base_url", "")
                                                error: controller.errorFor("llm.api_base_url")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("llm.api_base_url", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Timeout"
                                                value: controller.value("llm.timeout_ms", 15000)
                                                help: "Milliseconds."
                                                error: controller.errorFor("llm.timeout_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("llm.timeout_ms", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Provider sort"
                                                value: controller.value("llm.provider_sort", "")
                                                help: "Optional OpenRouter provider ordering expression."
                                                error: controller.errorFor("llm.provider_sort")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("llm.provider_sort", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            theme: theme
                                            title: "Agent context"

                                            SettingTextField {
                                                theme: theme
                                                label: "Context limit"
                                                value: controller.value("llm.agent_context_max_chars", 6000)
                                                help: "Maximum characters sent from a redacted agent-session excerpt."
                                                error: controller.errorFor("llm.agent_context_max_chars")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("llm.agent_context_max_chars", value);
                                                }
                                            }

                                        }

                                    }

                                }

                            }

                            SettingsPage {
                                id: outputPage

                                theme: theme
                                title: "Output"
                                description: "Control text delivery and input-method coordination."

                                SettingsGrid {
                                    SectionCard {
                                        theme: theme
                                        title: "Delivery"

                                        SettingCombo {
                                            theme: theme
                                            label: "Mode"
                                            value: controller.value("output.mode", "type")
                                            labels: ["Type", "Clipboard only", "Clipboard paste"]
                                            values: ["type", "clipboard", "paste"]
                                            error: controller.errorFor("output.mode")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("output.mode", value);
                                            }
                                        }

                                        SettingSwitch {
                                            theme: theme
                                            label: "Fallback to clipboard"
                                            checked: controller.value("output.fallback_to_clipboard", true)
                                            help: "Copy text when direct delivery fails."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("output.fallback_to_clipboard", checked);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "Input method"

                                        SettingSwitch {
                                            theme: theme
                                            label: "Manage Fcitx5"
                                            checked: controller.value("ime.manage_fcitx5", true)
                                            help: "Coordinate output with the active Fcitx5 input method."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("ime.manage_fcitx5", checked);
                                            }
                                        }

                                        SettingSwitch {
                                            theme: theme
                                            label: "Force ASCII before output"
                                            checked: controller.value("ime.force_ascii_before_output", true)
                                            help: "Switch to ASCII mode before inserting recognized text."
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("ime.force_ascii_before_output", checked);
                                            }
                                        }

                                    }

                                }

                                AdvancedSection {
                                    id: outputAdvanced

                                    theme: theme
                                    description: "Delivery timing, paste keys, and XWayland behavior."

                                    SettingsGrid {
                                        SectionCard {
                                            theme: theme
                                            title: "Timing and keys"

                                            SettingTextField {
                                                theme: theme
                                                label: "Type delay"
                                                value: controller.value("output.type_delay_ms", 0)
                                                help: "Milliseconds between generated key events."
                                                error: controller.errorFor("output.type_delay_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("output.type_delay_ms", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Pre-type delay"
                                                value: controller.value("output.pre_type_delay_ms", 140)
                                                help: "Milliseconds before delivery starts."
                                                error: controller.errorFor("output.pre_type_delay_ms")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("output.pre_type_delay_ms", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Paste keys"
                                                value: controller.value("output.paste_keys", "shift+Insert")
                                                error: controller.errorFor("output.paste_keys")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("output.paste_keys", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            theme: theme
                                            title: "XWayland"

                                            SettingSwitch {
                                                theme: theme
                                                label: "Prefer paste for XWayland"
                                                checked: controller.value("output.prefer_paste_for_xwayland", true)
                                                help: "Avoid garbled direct typing in XWayland clients."
                                                enabled: !controller.busy
                                                onToggled: (checked) => {
                                                    return controller.setValue("output.prefer_paste_for_xwayland", checked);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "XWayland paste keys"
                                                value: controller.value("output.xwayland_paste_keys", "shift+Insert")
                                                error: controller.errorFor("output.xwayland_paste_keys")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("output.xwayland_paste_keys", value);
                                                }
                                            }

                                        }

                                    }

                                }

                            }

                            SettingsPage {
                                id: appearancePage

                                theme: theme
                                title: "Appearance"
                                description: "Control HUD visibility and placement."

                                SettingsGrid {
                                    SectionCard {
                                        theme: theme
                                        title: "HUD"

                                        SettingSwitch {
                                            theme: theme
                                            label: "Enable HUD"
                                            checked: controller.value("hud.enabled", true)
                                            enabled: !controller.busy
                                            onToggled: (checked) => {
                                                return controller.setValue("hud.enabled", checked);
                                            }
                                        }

                                        SettingCombo {
                                            theme: theme
                                            label: "Position"
                                            value: controller.value("hud.position", "bottom-center")
                                            labels: ["Bottom center", "Bottom left", "Bottom right"]
                                            values: ["bottom-center", "bottom-left", "bottom-right"]
                                            error: controller.errorFor("hud.position")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("hud.position", value);
                                            }
                                        }

                                    }

                                }

                                AdvancedSection {
                                    id: appearanceAdvanced

                                    theme: theme
                                    description: "HUD geometry, offsets, and keyboard nudge distance."

                                    SettingsGrid {
                                        SectionCard {
                                            theme: theme
                                            title: "Geometry"

                                            SettingTextField {
                                                theme: theme
                                                label: "Bottom margin"
                                                value: controller.value("hud.margin_bottom", 72)
                                                help: "Pixels."
                                                error: controller.errorFor("hud.margin_bottom")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("hud.margin_bottom", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Base height"
                                                value: controller.value("hud.height", 56)
                                                help: "Pixels."
                                                error: controller.errorFor("hud.height")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("hud.height", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Horizontal offset"
                                                value: controller.value("hud.offset_x", 0)
                                                help: "Signed pixels."
                                                error: controller.errorFor("hud.offset_x")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("hud.offset_x", value);
                                                }
                                            }

                                        }

                                        SectionCard {
                                            theme: theme
                                            title: "Adjustment"

                                            SettingTextField {
                                                theme: theme
                                                label: "Vertical offset"
                                                value: controller.value("hud.offset_y", 0)
                                                help: "Signed pixels."
                                                error: controller.errorFor("hud.offset_y")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("hud.offset_y", value);
                                                }
                                            }

                                            SettingTextField {
                                                theme: theme
                                                label: "Nudge step"
                                                value: controller.value("hud.nudge_step", 24)
                                                help: "Signed pixel adjustment per nudge command."
                                                error: controller.errorFor("hud.nudge_step")
                                                enabled: !controller.busy
                                                onEdited: (value) => {
                                                    return controller.setValue("hud.nudge_step", value);
                                                }
                                            }

                                        }

                                    }

                                }

                            }

                            SettingsPage {
                                id: advancedPage

                                theme: theme
                                title: "Advanced"
                                description: "Configure the global trigger and state storage."

                                SettingsGrid {
                                    SectionCard {
                                        theme: theme
                                        title: "Hotkey"

                                        SettingTextField {
                                            theme: theme
                                            label: "Accelerator"
                                            value: controller.value("hotkey.accelerator", "")
                                            help: "Hyprland-style modifier and key description."
                                            error: controller.errorFor("hotkey.accelerator")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("hotkey.accelerator", value);
                                            }
                                        }

                                        SettingCombo {
                                            theme: theme
                                            label: "Mode"
                                            value: controller.value("hotkey.mode", "hold")
                                            labels: ["Hold", "Toggle"]
                                            values: ["hold", "toggle"]
                                            error: controller.errorFor("hotkey.mode")
                                            enabled: !controller.busy
                                            onSelected: (value) => {
                                                return controller.setValue("hotkey.mode", value);
                                            }
                                        }

                                    }

                                    SectionCard {
                                        theme: theme
                                        title: "State"

                                        SettingTextField {
                                            theme: theme
                                            label: "State file"
                                            value: controller.value("state_file", "auto")
                                            help: "Use auto, disabled, or an absolute custom path."
                                            error: controller.errorFor("state_file")
                                            enabled: !controller.busy
                                            onEdited: (value) => {
                                                return controller.setValue("state_file", value);
                                            }
                                        }

                                    }

                                }

                            }

                        }

                    }

                    Rectangle {
                        Layout.fillWidth: true
                        Layout.preferredHeight: 58
                        color: theme.surface
                        border.color: theme.border

                        RowLayout {
                            id: footerLayout

                            anchors.fill: parent
                            anchors.margins: 9
                            spacing: 8

                            ColumnLayout {
                                Layout.fillWidth: true
                                spacing: 2

                                Label {
                                    visible: controller.globalError.length > 0
                                    text: theme.tr(controller.globalError)
                                    color: theme.error
                                    font.pixelSize: 11
                                    font.weight: Font.Medium
                                    wrapMode: Text.WordWrap
                                    Layout.fillWidth: true
                                    Accessible.role: Accessible.AlertMessage
                                }

                                Label {
                                    visible: controller.globalError.length === 0 && controller.statusMessage.length > 0
                                    text: theme.tr(controller.statusMessage)
                                    color: theme.success
                                    font.pixelSize: 11
                                    wrapMode: Text.WordWrap
                                    Layout.fillWidth: true
                                }

                                Label {
                                    visible: controller.globalError.length === 0 && controller.statusMessage.length === 0
                                    text: theme.tr(controller.saving ? "Saving configuration and restarting service…" : (controller.loading ? "Reloading configuration…" : (controller.testing ? "Testing LLM settings…" : (controller.dirty ? "Changes have not been saved." : "Configuration is up to date."))))
                                    color: controller.dirty ? theme.warning : theme.subtle
                                    font.pixelSize: 11
                                }

                            }

                            AppButton {
                                theme: theme
                                text: "Discard"
                                enabled: controller.dirty && !controller.busy
                                onClicked: controller.discard()
                            }

                            AppButton {
                                theme: theme
                                text: controller.saving ? "Saving…" : "Save & restart"
                                primary: true
                                enabled: controller.dirty && !controller.busy
                                onClicked: controller.save()
                            }

                        }

                    }

                }

            }

        }

        Dialog {
            id: closeDialog

            parent: window.contentItem
            width: 440
            height: 180
            x: Math.round((parent.width - width) / 2)
            y: Math.round((parent.height - height) / 2)
            modal: true
            title: theme.tr("Discard unsaved changes?")
            standardButtons: Dialog.NoButton

            background: Rectangle {
                color: theme.surface
                border.color: theme.border
                radius: 12
            }

            contentItem: ColumnLayout {
                spacing: 18

                Label {
                    text: theme.tr("Your configuration or credential replacements have not been saved.")
                    color: theme.foreground
                    wrapMode: Text.WordWrap
                    Layout.preferredWidth: 380
                }

                RowLayout {
                    Layout.alignment: Qt.AlignRight

                    AppButton {
                        theme: theme
                        text: "Keep editing"
                        onClicked: closeDialog.close()
                    }

                    AppButton {
                        theme: theme
                        text: "Discard and close"
                        danger: true
                        onClicked: {
                            controller.discard();
                            closeDialog.close();
                            Qt.quit();
                        }
                    }

                }

            }

        }

    }

}

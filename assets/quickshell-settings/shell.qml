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
        destinationSidebar.currentIndex = destinationIndex(title);
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

                SettingsSidebar {
                    id: destinationSidebar

                    theme: theme
                    controller: controller
                    destinations: shell.destinations
                    serviceRunning: shell.serviceRunning()
                    serviceSummary: shell.serviceSummary()
                    onNavigateRequested: (page) => {
                        return shell.navigate(page);
                    }
                }

                ColumnLayout {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    spacing: 0

                    SettingsHeader {
                        theme: theme
                        controller: controller
                        title: shell.destinations[Math.max(0, destinationSidebar.currentIndex)].title
                        onCloseRequested: window.requestClose()
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
                            currentIndex: destinationSidebar.currentIndex

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

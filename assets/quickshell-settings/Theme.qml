import QtQuick
import Quickshell
import Quickshell.Io

QtObject {
    // Keep the last valid palette.

    id: root

    readonly property string themePath: Quickshell.env("HOME") + "/.config/omarchy/current/theme/colors.toml"
    // Safe, high-contrast defaults are retained if the Omarchy theme is missing
    // or is being replaced while this window is open.
    property var palette: ({
        "accent": "#7dd3fc",
        "foreground": "#eef2ff",
        "background": "#101827",
        "surface": "#182338",
        "elevated": "#24324a",
        "muted": "#a8b3c7",
        "error": "#fb7185",
        "success": "#82aaa1",
        "warning": "#facc15"
    })
    readonly property color accent: palette.accent
    readonly property color foreground: palette.foreground
    readonly property color background: palette.background
    readonly property color surface: palette.surface
    readonly property color elevated: palette.elevated
    readonly property color muted: palette.muted
    readonly property color error: palette.error
    readonly property color success: palette.success
    readonly property color warning: palette.warning
    readonly property color border: Qt.alpha(foreground, 0.14)
    readonly property color subtle: Qt.alpha(foreground, 0.68)
    property I18n i18n
    property FileView themeFile
    property Timer refreshTimer

    function tr(text) {
        return i18n.tr(text);
    }

    function readColor(source, key, fallback) {
        const pattern = new RegExp("^\\s*" + key + "\\s*=\\s*[\\\"'](#[0-9a-fA-F]{6,8})[\\\"']", "m");
        const match = source.match(pattern);
        return match ? match[1] : fallback;
    }

    function refresh() {
        try {
            themeFile.reload();
            const source = themeFile.text();
            palette = {
                "accent": readColor(source, "accent", palette.accent),
                "foreground": readColor(source, "foreground", palette.foreground),
                "background": readColor(source, "background", palette.background),
                "surface": readColor(source, "surface", palette.surface),
                "elevated": readColor(source, "elevated", palette.elevated),
                "muted": readColor(source, "color8", palette.muted),
                "error": readColor(source, "color1", palette.error),
                "success": readColor(source, "color2", palette.success),
                "warning": readColor(source, "color3", palette.warning)
            };
        } catch (error) {
        }
    }

    i18n: I18n {
    }

    themeFile: FileView {
        path: root.themePath
        watchChanges: true
        blockAllReads: true
        printErrors: false
        onFileChanged: root.refresh()
    }

    refreshTimer: Timer {
        interval: 1500
        running: true
        repeat: true
        triggeredOnStart: true
        onTriggered: root.refresh()
    }

}

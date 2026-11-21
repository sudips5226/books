import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs

import com.pdf2word.app

ApplicationWindow {
    id: window
    width: 1240
    height: 840
    minimumWidth: 960
    minimumHeight: 620
    visible: true
    title: conv.inputPath.length > 0 ? "PDF to Word — " + conv.inputPath.split(/[\\/]/).pop() : "PDF to Word"

    readonly property color accent: "#2b579a"
    readonly property bool dark: palette.window.hslLightness < 0.5
    readonly property color subtle: dark ? "#9aa4b2" : "#5f6b7a"
    readonly property var queue: JSON.parse(conv.queueJson)
    readonly property var structure: JSON.parse(conv.structureJson)
    readonly property var pageText: JSON.parse(conv.pageTextJson)
    readonly property var textItems: pageText.items || []
    readonly property var paragraphs: pageText.paragraphs || []
    // Edit whole paragraphs instead of single lines.
    property bool paragraphMode: false
    // Runs selected with Ctrl/Shift-click.
    property var selection: []
    // The run (or paragraph) in the on-page editor, -1 for none.
    property int editIndex: -1
    // Open the editor on this run once the page has been read again (Tab).
    property int nextAfterEdit: -1
    readonly property var editTarget: editIndex < 0 ? null : (paragraphMode ? paragraphs[editIndex] : textItems[editIndex])

    function openEditor(i) {
        var t = paragraphMode ? paragraphs[i] : textItems[i]
        if (!t)
            return
        editIndex = i
        editor.load(t)
    }
    function closeEditor(cancelPreview) {
        editIndex = -1
        if (cancelPreview)
            conv.cancelPreview()
    }
    function toggleSelected(i) {
        var s = selection.slice()
        var k = s.indexOf(i)
        if (k >= 0) s.splice(k, 1); else s.push(i)
        selection = s
    }
    function esc(t) {
        return String(t).replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;")
    }
    onPageTextChanged: {
        selection = []
        if (nextAfterEdit >= 0) {
            var n = nextAfterEdit
            nextAfterEdit = -1
            if (n < (paragraphMode ? paragraphs.length : textItems.length))
                openEditor(n)
            else
                editIndex = -1
        } else if (editIndex >= 0 && !conv.previewing) {
            editIndex = -1
        }
    }

    // Colours of the structure overlay.
    readonly property var kindColors: ({
        "heading": "#8e44ad", "paragraph": "#2b78e4", "list": "#16a085", "table": "#27ae60",
        "column": "#95a5a6", "picture": "#e67e22", "figure": "#d35400", "equation": "#c0392b",
        "rule": "#7f8c8d", "textbox": "#f1c40f", "comment": "#e84393"
    })
    readonly property var kindNames: [
        ["heading", "Heading"], ["paragraph", "Paragraph"], ["list", "List item"], ["table", "Table"],
        ["column", "Column"], ["picture", "Picture"], ["figure", "Editable figure"], ["equation", "Equation"],
        ["textbox", "Text box"], ["comment", "Comment"]
    ]

    onClosing: conv.saveSettings()

    Converter {
        id: conv
        Component.onCompleted: openFromArguments()
        onFinished: (success, message) => {
            if (success) {
                doneLabel.text = "✔ " + message
                doneBanner.open()
            }
        }
    }

    FileDialog {
        id: openDialog
        title: "Open PDF files"
        fileMode: FileDialog.OpenFiles
        nameFilters: ["PDF documents (*.pdf)", "All files (*)"]
        onAccepted: conv.addUrls(selectedFiles)
    }

    FileDialog {
        id: saveDialog
        title: "Save Word document as"
        fileMode: FileDialog.SaveFile
        defaultSuffix: "docx"
        nameFilters: ["Word documents (*.docx)"]
        onAccepted: conv.setOutputUrl(selectedFile)
    }

    FolderDialog {
        id: folderDialog
        title: "Folder for the Word documents"
        onAccepted: conv.setOutputFolderUrl(selectedFolder)
    }

    FileDialog {
        id: savePdfDialog
        title: "Save the edited PDF as"
        fileMode: FileDialog.SaveFile
        defaultSuffix: "pdf"
        nameFilters: ["PDF documents (*.pdf)"]
        onAccepted: conv.saveEditedPdf(selectedFile)
    }

    // Add new text where the page was clicked.
    Dialog {
        id: insertDialog
        property real px: 0
        property real py: 0
        title: "Add text"
        modal: true
        anchors.centerIn: parent
        width: Math.min(window.width - 80, 560)
        standardButtons: Dialog.Ok | Dialog.Cancel
        onOpened: { insertField.text = ""; insertField.forceActiveFocus() }
        onAccepted: conv.insertText(px, py, insertField.text, insertSize.value,
                                     insertFamily.currentIndex + (insertBold.checked ? 4 : 0) + (insertItalic.checked ? 8 : 0))

        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            TextField {
                id: insertField
                Layout.fillWidth: true
                placeholderText: "New text"
                onAccepted: insertDialog.accept()
            }
            RowLayout {
                spacing: 8
                ComboBox {
                    id: insertFamily
                    model: ["Sans-serif", "Serif", "Monospace"]
                }
                SpinBox {
                    id: insertSize
                    from: 4
                    to: 96
                    value: 11
                    editable: true
                }
                Label { text: "pt" }
                CheckBox { id: insertBold; text: "Bold" }
                CheckBox { id: insertItalic; text: "Italic" }
            }
        }
    }

    // Find, tick the matches to change, replace.
    Dialog {
        id: findDialog
        title: "Find and replace in the PDF"
        modal: false
        anchors.centerIn: parent
        width: Math.min(window.width - 80, 680)
        height: Math.min(window.height - 80, 540)
        standardButtons: Dialog.Close
        readonly property var results: JSON.parse(conv.findJson)
        property var unticked: ({})
        onOpened: findField.forceActiveFocus()
        onResultsChanged: unticked = ({})
        function search() {
            if (findField.text.length > 0)
                conv.findText(findField.text, matchCase.checked, wholeWord.checked)
        }
        function ticked() {
            var v = []
            for (var k = 0; k < results.length; k++)
                if (!unticked[results[k].i]) v.push(results[k].i)
            return v
        }

        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            RowLayout {
                TextField { id: findField; Layout.fillWidth: true; placeholderText: "Find"; onAccepted: findDialog.search() }
                Button { text: "Find"; enabled: !conv.findBusy && findField.text.length > 0; onClicked: findDialog.search() }
            }
            RowLayout {
                CheckBox { id: matchCase; text: "Match case" }
                CheckBox { id: wholeWord; text: "Whole words" }
                Item { Layout.fillWidth: true }
                BusyIndicator { running: conv.findBusy; visible: running; implicitHeight: 24; implicitWidth: 24 }
                Label {
                    color: window.subtle
                    text: findDialog.results.length === 0 ? "" : findDialog.results.length + (findDialog.results.length === 1 ? " match" : " matches")
                }
            }
            ListView {
                Layout.fillWidth: true
                Layout.fillHeight: true
                clip: true
                model: findDialog.results
                ScrollBar.vertical: ScrollBar {}
                delegate: CheckDelegate {
                    required property var modelData
                    width: ListView.view.width
                    checked: !findDialog.unticked[modelData.i]
                    onToggled: {
                        var u = Object.assign({}, findDialog.unticked)
                        u[modelData.i] = !checked
                        findDialog.unticked = u
                    }
                    contentItem: Label {
                        rightPadding: 28
                        textFormat: Text.StyledText
                        elide: Text.ElideRight
                        text: "<font color='" + window.subtle + "'>p. " + modelData.page + "</font>  …" + window.esc(modelData.before)
                              + "<b>" + window.esc(modelData.found) + "</b>" + window.esc(modelData.after) + "…"
                    }
                }
            }
            RowLayout {
                TextField { id: replaceField; Layout.fillWidth: true; placeholderText: "Replace with" }
                Button {
                    text: "Replace ticked"
                    enabled: findDialog.results.length > 0 && !conv.editBusy && !conv.busy
                    onClicked: conv.replaceFound(JSON.stringify(findDialog.ticked()), replaceField.text)
                }
            }
        }
    }

    function statusText(s) {
        return s === "waiting" ? "Waiting" : s === "converting" ? "Converting…" : s === "done" ? "Done" : "Failed"
    }
    function statusColor(s) {
        return s === "done" ? "#2e9d57" : s === "failed" ? "#d64545" : s === "converting" ? accent : subtle
    }

    header: ToolBar {
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: 8
            anchors.rightMargin: 8
            spacing: 8

            ToolButton {
                text: "Open PDF…"
                icon.name: "document-open"
                enabled: !conv.busy
                onClicked: openDialog.open()
            }
            Label {
                text: conv.documentInfo
                elide: Text.ElideRight
                color: window.subtle
                Layout.fillWidth: true
            }
            Button {
                text: conv.busy ? "Cancel" : (conv.pending > 1 ? "Convert " + conv.pending + " files" : "Convert to Word")
                highlighted: !conv.busy
                enabled: conv.busy || conv.pending > 0
                onClicked: conv.busy ? conv.cancel() : conv.convert()
            }
        }
    }

    SplitView {
        anchors.fill: parent
        orientation: Qt.Horizontal

        // ------------------------------------------------------------ preview
        Pane {
            SplitView.fillWidth: true
            SplitView.minimumWidth: 420
            padding: 12

            ColumnLayout {
                anchors.fill: parent
                spacing: 8

                Rectangle {
                    id: stage
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    color: window.dark ? "#2a2d33" : "#e6e9ee"
                    radius: 6

                    Image {
                        id: preview
                        anchors.fill: parent
                        anchors.margins: 16
                        source: conv.previewSource
                        fillMode: Image.PreserveAspectFit
                        asynchronous: true
                        cache: false
                        smooth: true
                        mipmap: true
                    }

                    // Detected structure over the page.
                    Item {
                        id: overlay
                        visible: conv.showStructure && !conv.editMode && preview.status === Image.Ready
                        x: preview.x + (preview.width - preview.paintedWidth) / 2
                        y: preview.y + (preview.height - preview.paintedHeight) / 2
                        width: preview.paintedWidth
                        height: preview.paintedHeight

                        Repeater {
                            model: window.structure
                            delegate: Rectangle {
                                required property var modelData
                                readonly property color kindColor: window.kindColors[modelData.kind] || "#888888"
                                x: modelData.x * overlay.width - 1
                                y: modelData.y * overlay.height - 1
                                width: Math.max(3, modelData.w * overlay.width + 2)
                                height: Math.max(3, modelData.h * overlay.height + 2)
                                color: Qt.rgba(kindColor.r, kindColor.g, kindColor.b, modelData.kind === "column" ? 0.04 : 0.14)
                                border.color: kindColor
                                border.width: modelData.kind === "column" ? 1 : 1.5
                                z: modelData.kind === "column" ? 0 : 1

                                MouseArea {
                                    id: hover
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    acceptedButtons: Qt.NoButton
                                }
                                ToolTip.visible: hover.containsMouse
                                ToolTip.text: modelData.label
                                ToolTip.delay: 300
                            }
                        }
                    }

                    // Text runs (or paragraphs) that can be edited.
                    Item {
                        id: editOverlay
                        visible: conv.editMode && preview.status === Image.Ready
                        x: preview.x + (preview.width - preview.paintedWidth) / 2
                        y: preview.y + (preview.height - preview.paintedHeight) / 2
                        width: preview.paintedWidth
                        height: preview.paintedHeight
                        enabled: !conv.busy

                        // A click on empty space adds text there.
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.IBeamCursor
                            onClicked: (mouse) => {
                                if (window.editIndex >= 0) {
                                    window.closeEditor(true)
                                    return
                                }
                                window.selection = []
                                insertDialog.px = mouse.x / width
                                insertDialog.py = mouse.y / height
                                insertDialog.open()
                            }
                        }

                        Repeater {
                            model: window.paragraphMode ? window.paragraphs : window.textItems
                            delegate: Rectangle {
                                id: box
                                required property var modelData
                                required property int index
                                readonly property bool outlined: !window.paragraphMode && modelData.outlined === true
                                readonly property bool selected: !window.paragraphMode && window.selection.indexOf(index) >= 0
                                readonly property bool current: window.editIndex === index
                                property real dragX: 0
                                property real dragY: 0
                                x: modelData.x * editOverlay.width - 2 + dragX
                                y: modelData.y * editOverlay.height - 2 + dragY
                                width: Math.max(4, modelData.w * editOverlay.width + 4)
                                height: Math.max(4, modelData.h * editOverlay.height + 4)
                                radius: 2
                                readonly property color tint: outlined ? "#e67e22" : window.accent
                                color: Qt.rgba(tint.r, tint.g, tint.b, selected || current ? 0.22 : (textHover.containsMouse ? 0.16 : 0.05))
                                border.color: selected || current || textHover.containsMouse ? tint : Qt.rgba(tint.r, tint.g, tint.b, 0.4)
                                border.width: selected || current ? 2 : 1

                                MouseArea {
                                    id: textHover
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: dragging ? Qt.ClosedHandCursor : Qt.PointingHandCursor
                                    property point start
                                    property bool dragging: false
                                    onPressed: (mouse) => { start = Qt.point(mouse.x, mouse.y); dragging = false }
                                    onPositionChanged: (mouse) => {
                                        if (!pressed || window.paragraphMode || box.outlined || conv.editBusy)
                                            return
                                        var dx = mouse.x - start.x, dy = mouse.y - start.y
                                        if (!dragging && Math.abs(dx) + Math.abs(dy) > 5)
                                            dragging = true
                                        if (dragging) { box.dragX = dx; box.dragY = dy }
                                    }
                                    onReleased: (mouse) => {
                                        if (dragging) {
                                            conv.moveText(box.index, box.dragX / editOverlay.width, box.dragY / editOverlay.height)
                                            dragging = false
                                            return
                                        }
                                    }
                                    onClicked: (mouse) => {
                                        if (box.dragX !== 0 || box.dragY !== 0)
                                            return
                                        if (!window.paragraphMode && (mouse.modifiers & (Qt.ControlModifier | Qt.ShiftModifier | Qt.MetaModifier))) {
                                            window.toggleSelected(box.index)
                                            return
                                        }
                                        window.selection = []
                                        window.openEditor(box.index)
                                    }
                                }
                                ToolTip.visible: textHover.containsMouse && window.editIndex < 0
                                ToolTip.text: window.paragraphMode
                                              ? "Paragraph, " + modelData.lines + (modelData.lines === 1 ? " line" : " lines") + " — click to edit"
                                              : modelData.font + " · " + modelData.size + " pt — click to edit, drag to move, Ctrl-click to select"
                                ToolTip.delay: 500
                            }
                        }
                    }

                    // The on-page editor, next to the text being changed.
                    Pane {
                        id: editor
                        visible: conv.editMode && window.editTarget !== null && window.editTarget !== undefined
                        z: 20
                        padding: 10
                        property var item: ({})
                        property bool sizeTouched: false
                        property string pickedColor: ""
                        readonly property bool para: window.paragraphMode
                        readonly property real tx: editOverlay.x + (window.editTarget ? window.editTarget.x * editOverlay.width : 0)
                        readonly property real ty: editOverlay.y + (window.editTarget ? window.editTarget.y * editOverlay.height : 0)
                        readonly property real th: window.editTarget ? window.editTarget.h * editOverlay.height : 0
                        width: Math.min(stage.width - 8, Math.max(editorColumn.implicitWidth + 2 * padding, para ? 480 : 420, (window.editTarget ? window.editTarget.w * editOverlay.width : 0) + 40))
                        x: Math.max(4, Math.min(tx - 10, stage.width - width - 4))
                        // Above the text when there is room, else below it.
                        y: ty - height - 8 >= 0 ? ty - height - 8 : Math.max(4, Math.min(stage.height - height - 4, ty + th + 8))
                        background: Rectangle { color: window.palette.window; border.color: window.accent; border.width: 1.5; radius: 6 }

                        function load(t) {
                            item = t
                            sizeTouched = false
                            pickedColor = ""
                            lineField.text = t.text
                            paraField.text = t.text
                            boldBtn.checked = t.bold === true
                            italicBtn.checked = t.italic === true
                            sizeBox.value = Math.round(t.size || 11)
                            if (para) paraField.forceActiveFocus(); else { lineField.forceActiveFocus(); lineField.selectAll() }
                        }
                        function text() { return para ? paraField.text : lineField.text }
                        function styleJson() {
                            var st = {}
                            if (boldBtn.checked !== (item.bold === true)) st.bold = boldBtn.checked
                            if (italicBtn.checked !== (item.italic === true)) st.italic = italicBtn.checked
                            if (sizeTouched) st.size = sizeBox.value
                            if (pickedColor.length > 0) st.color = pickedColor
                            return JSON.stringify(st)
                        }
                        function changed() {
                            return text() !== item.text || styleJson() !== "{}"
                        }
                        function schedulePreview() {
                            if (visible && changed()) previewTimer.restart()
                        }
                        function apply(next) {
                            previewTimer.stop()
                            if (!changed()) {
                                if (next >= 0) window.openEditor(next); else window.closeEditor(true)
                                return
                            }
                            window.nextAfterEdit = next
                            if (para)
                                conv.editParagraph(window.editIndex, paraField.text, fitBox.checked, pushBox.checked)
                            else
                                conv.editRun(window.editIndex, lineField.text, fitBox.checked, styleJson())
                            if (next < 0) window.editIndex = -1
                        }
                        Timer {
                            id: previewTimer
                            interval: 700
                            onTriggered: {
                                if (!editor.visible) return
                                if (editor.para)
                                    conv.previewParagraph(window.editIndex, paraField.text, fitBox.checked, pushBox.checked)
                                else
                                    conv.previewRun(window.editIndex, lineField.text, fitBox.checked, editor.styleJson())
                            }
                        }

                        ColumnLayout {
                            id: editorColumn
                            anchors.fill: parent
                            spacing: 6
                            RowLayout {
                                Label {
                                    Layout.fillWidth: true
                                    elide: Text.ElideRight
                                    color: window.subtle
                                    font.pointSize: 9
                                    text: editor.para
                                          ? "Paragraph · " + (editor.item.lines || 1) + " lines · " + (editor.item.align || "")
                                          : (editor.item.font || "") + " · " + (editor.item.size || 0) + " pt" + (editor.item.outlined ? " · recognised from outlines" : "")
                                }
                                Label {
                                    visible: conv.previewing
                                    text: "Preview"
                                    color: "white"
                                    font.pointSize: 8
                                    padding: 2
                                    leftPadding: 6
                                    rightPadding: 6
                                    background: Rectangle { color: window.accent; radius: 3 }
                                }
                            }
                            TextField {
                                id: lineField
                                visible: !editor.para
                                Layout.fillWidth: true
                                Layout.preferredWidth: 380
                                font.bold: boldBtn.checked
                                font.italic: italicBtn.checked
                                horizontalAlignment: editor.item.rtl ? Text.AlignRight : Text.AlignLeft
                                onTextEdited: editor.schedulePreview()
                                onAccepted: editor.apply(-1)
                                Keys.onEscapePressed: window.closeEditor(true)
                                Keys.onTabPressed: editor.apply(window.editIndex + 1 < window.textItems.length ? window.editIndex + 1 : -1)
                                Keys.onBacktabPressed: editor.apply(window.editIndex > 0 ? window.editIndex - 1 : -1)
                            }
                            ScrollView {
                                id: paraScroll
                                visible: editor.para
                                Layout.fillWidth: true
                                Layout.preferredWidth: 440
                                Layout.preferredHeight: 130
                                contentWidth: availableWidth
                                TextArea {
                                    id: paraField
                                    wrapMode: TextEdit.Wrap
                                    horizontalAlignment: editor.item.align === "right" ? Text.AlignRight : Text.AlignLeft
                                    onTextChanged: editor.schedulePreview()
                                    Keys.onEscapePressed: window.closeEditor(true)
                                    Keys.onReturnPressed: (event) => {
                                        if (event.modifiers & Qt.ControlModifier) editor.apply(-1); else event.accepted = false
                                    }
                                }
                            }
                            RowLayout {
                                spacing: 4
                                visible: !editor.para
                                ToolButton { id: boldBtn; text: "B"; font.bold: true; checkable: true; onToggled: editor.schedulePreview(); ToolTip.text: "Bold"; ToolTip.visible: hovered }
                                ToolButton { id: italicBtn; text: "I"; font.italic: true; checkable: true; onToggled: editor.schedulePreview(); ToolTip.text: "Italic"; ToolTip.visible: hovered }
                                SpinBox {
                                    id: sizeBox
                                    from: 4
                                    to: 144
                                    editable: true
                                    implicitWidth: 96
                                    onValueModified: { editor.sizeTouched = true; editor.schedulePreview() }
                                }
                                Label { text: "pt"; color: window.subtle }
                                Repeater {
                                    model: ["#000000", "#1f4e79", "#c00000", "#2e7d32", "#7030a0", "#808080"]
                                    delegate: Rectangle {
                                        required property string modelData
                                        width: 18
                                        height: 18
                                        radius: 9
                                        color: modelData
                                        border.width: editor.pickedColor === modelData ? 3 : 1
                                        border.color: editor.pickedColor === modelData ? window.accent : "#999999"
                                        MouseArea {
                                            anchors.fill: parent
                                            onClicked: { editor.pickedColor = editor.pickedColor === parent.modelData ? "" : parent.modelData; editor.schedulePreview() }
                                        }
                                    }
                                }
                            }
                            RowLayout {
                                CheckBox {
                                    id: fitBox
                                    text: editor.para ? "Make smaller to fit" : "Keep within the width"
                                    checked: !editor.para
                                    onToggled: editor.schedulePreview()
                                }
                                CheckBox {
                                    id: pushBox
                                    visible: editor.para
                                    text: "Move the text below down"
                                    checked: true
                                    onToggled: editor.schedulePreview()
                                }
                                Item { Layout.fillWidth: true }
                                Button {
                                    text: "Delete"
                                    visible: !editor.para
                                    onClicked: { var i = window.editIndex; window.closeEditor(false); conv.deleteItems(JSON.stringify([i])) }
                                }
                                Button { text: "Cancel"; onClicked: window.closeEditor(true) }
                                Button { text: "Apply"; highlighted: true; enabled: !conv.editBusy; onClicked: editor.apply(-1) }
                            }
                            Label {
                                Layout.fillWidth: true
                                wrapMode: Text.WordWrap
                                font.pointSize: 8
                                color: window.subtle
                                text: editor.para ? "Ctrl+Enter applies · Esc cancels · the paragraph wraps over as many lines as it needs"
                                                  : "Enter applies · Tab applies and edits the next line · Esc cancels"
                            }
                        }
                    }

                    BusyIndicator {
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.margins: 8
                        running: conv.editBusy || conv.ocrBusy
                        visible: running
                    }

                    Column {
                        anchors.centerIn: parent
                        spacing: 12
                        visible: window.queue.length === 0
                        Label {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "📄"
                            font.pixelSize: 64
                        }
                        Label {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "Drop PDF files here"
                            font.pixelSize: 18
                        }
                        Button {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "Browse…"
                            onClicked: openDialog.open()
                        }
                    }

                    BusyIndicator {
                        anchors.centerIn: parent
                        running: conv.pageCount > 0 && preview.status !== Image.Ready
                        visible: running
                    }

                    // Drag & drop of PDF files.
                    DropArea {
                        anchors.fill: parent
                        keys: ["text/uri-list"]
                        onEntered: (drag) => dropHighlight.visible = true
                        onExited: dropHighlight.visible = false
                        onDropped: (drop) => {
                            dropHighlight.visible = false
                            if (drop.hasUrls && drop.urls.length > 0)
                                conv.addUrls(drop.urls)
                        }
                    }
                    Rectangle {
                        id: dropHighlight
                        anchors.fill: parent
                        visible: false
                        color: "transparent"
                        border.color: window.accent
                        border.width: 3
                        radius: 6
                    }
                }

                // Editing tools.
                RowLayout {
                    Layout.fillWidth: true
                    visible: conv.editMode && conv.pageCount > 0
                    spacing: 6
                    ButtonGroup { id: unitGroup }
                    Button {
                        text: "Lines"
                        checkable: true
                        checked: !window.paragraphMode
                        ButtonGroup.group: unitGroup
                        onClicked: { window.closeEditor(true); window.paragraphMode = false }
                    }
                    Button {
                        text: "Paragraphs"
                        checkable: true
                        checked: window.paragraphMode
                        ButtonGroup.group: unitGroup
                        onClicked: { window.closeEditor(true); window.paragraphMode = true }
                    }
                    Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        color: window.subtle
                        text: window.selection.length > 0 ? window.selection.length + " selected"
                              : conv.editCount > 0 ? conv.editCount + (conv.editCount === 1 ? " change" : " changes") + " — converting uses the edited PDF"
                              : "Click text to change it, empty space to add text"
                    }
                    Button {
                        text: conv.ocrBusy ? "Recognising…" : "Recognise outlined text"
                        visible: (window.pageText.outlined || 0) > 0 && !window.pageText.recognised
                        enabled: !conv.ocrBusy
                        onClicked: conv.recognizeOutlined()
                        ToolTip.text: "Letters on this page are drawn as shapes. Recognise them (OCR) to edit them."
                        ToolTip.visible: hovered
                    }
                    Button {
                        text: "Delete selected"
                        visible: window.selection.length > 0
                        enabled: !conv.editBusy
                        onClicked: { conv.deleteItems(JSON.stringify(window.selection)); window.selection = [] }
                    }
                    Button {
                        text: "Find and replace…"
                        enabled: !conv.busy
                        onClicked: findDialog.open()
                    }
                    Button {
                        text: "Undo"
                        enabled: conv.editCount > 0 && !conv.editBusy && !conv.busy
                        onClicked: { window.closeEditor(false); conv.undoEdit() }
                    }
                    Button {
                        text: "Redo"
                        enabled: conv.redoCount > 0 && !conv.editBusy && !conv.busy
                        onClicked: { window.closeEditor(false); conv.redoEdit() }
                    }
                    Button {
                        text: "Save PDF…"
                        enabled: conv.editCount > 0 && !conv.editBusy
                        onClicked: savePdfDialog.open()
                    }
                }
                Shortcut {
                    sequences: [StandardKey.Undo]
                    enabled: conv.editMode && window.editIndex < 0 && conv.editCount > 0
                    onActivated: conv.undoEdit()
                }
                Shortcut {
                    sequences: [StandardKey.Redo]
                    enabled: conv.editMode && window.editIndex < 0 && conv.redoCount > 0
                    onActivated: conv.redoEdit()
                }
                Shortcut {
                    sequences: [StandardKey.Delete]
                    enabled: conv.editMode && window.editIndex < 0 && window.selection.length > 0
                    onActivated: { conv.deleteItems(JSON.stringify(window.selection)); window.selection = [] }
                }

                // Legend of the overlay.
                Flow {
                    Layout.fillWidth: true
                    spacing: 10
                    visible: conv.showStructure && !conv.editMode && conv.pageCount > 0
                    Repeater {
                        model: window.kindNames
                        delegate: Row {
                            required property var modelData
                            spacing: 4
                            Rectangle {
                                width: 12
                                height: 12
                                anchors.verticalCenter: parent.verticalCenter
                                color: "transparent"
                                border.width: 2
                                border.color: window.kindColors[modelData[0]]
                            }
                            Label {
                                text: modelData[1]
                                font.pointSize: 9
                                color: window.subtle
                            }
                        }
                    }
                }

                RowLayout {
                    Layout.fillWidth: true
                    spacing: 6
                    CheckBox {
                        text: "Show detected structure"
                        checked: conv.showStructure
                        enabled: conv.pageCount > 0 && !conv.editMode
                        onToggled: conv.setStructureVisible(checked)
                    }
                    CheckBox {
                        text: "Edit text"
                        checked: conv.editMode
                        enabled: conv.pageCount > 0
                        onToggled: conv.enableEditing(checked)
                    }
                    Item { Layout.fillWidth: true }
                    RowLayout {
                        enabled: conv.pageCount > 0
                        spacing: 6
                        Button {
                            text: "◀"
                            enabled: conv.previewPage > 1
                            onClicked: conv.showPage(conv.previewPage - 1)
                        }
                        SpinBox {
                            from: 1
                            to: Math.max(1, conv.pageCount)
                            value: conv.previewPage
                            editable: true
                            onValueModified: conv.showPage(value)
                        }
                        Label { text: "of " + conv.pageCount }
                        Button {
                            text: "▶"
                            enabled: conv.previewPage < conv.pageCount
                            onClicked: conv.showPage(conv.previewPage + 1)
                        }
                    }
                    Item { Layout.fillWidth: true }
                }
            }
        }

        // ------------------------------------------------------------ options
        ScrollView {
            id: optionsView
            SplitView.preferredWidth: 420
            SplitView.minimumWidth: 340
            contentWidth: availableWidth
            background: Rectangle { color: window.palette.window }

            ColumnLayout {
                width: optionsView.availableWidth
                spacing: 12

                Item { Layout.preferredHeight: 4 }

                GroupBox {
                    title: window.queue.length > 1 ? "Files (" + window.queue.length + ")" : "Files"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    ColumnLayout {
                        anchors.fill: parent
                        spacing: 6

                        ListView {
                            id: queueView
                            Layout.fillWidth: true
                            Layout.preferredHeight: Math.min(Math.max(contentHeight, 36), 210)
                            clip: true
                            model: window.queue
                            ScrollBar.vertical: ScrollBar {}
                            delegate: ItemDelegate {
                                id: row
                                required property var modelData
                                required property int index
                                width: ListView.view.width
                                highlighted: index === conv.selected
                                onClicked: conv.selectItem(index)
                                ToolTip.visible: hovered && modelData.message.length > 0
                                ToolTip.text: modelData.message
                                ToolTip.delay: 500
                                contentItem: ColumnLayout {
                                    spacing: 2
                                    RowLayout {
                                        spacing: 6
                                        Label {
                                            text: row.modelData.name
                                            elide: Text.ElideMiddle
                                            Layout.fillWidth: true
                                        }
                                        Label {
                                            visible: row.modelData.edits > 0
                                            text: "✎ " + row.modelData.edits
                                            color: row.highlighted ? row.palette.highlightedText : window.accent
                                            font.pointSize: 9
                                        }
                                        Label {
                                            text: window.statusText(row.modelData.status)
                                            color: row.highlighted ? row.palette.highlightedText : window.statusColor(row.modelData.status)
                                            font.pointSize: 9
                                        }
                                        ToolButton {
                                            text: "✕"
                                            enabled: row.modelData.status !== "converting"
                                            implicitWidth: 26
                                            implicitHeight: 22
                                            onClicked: conv.removeItem(row.index)
                                        }
                                    }
                                    ProgressBar {
                                        Layout.fillWidth: true
                                        visible: row.modelData.status === "converting"
                                        value: row.modelData.progress
                                    }
                                }
                            }
                        }
                        Label {
                            visible: window.queue.length === 0
                            text: "No files yet — open or drop PDFs; several files are converted one after the other."
                            wrapMode: Text.WordWrap
                            color: window.subtle
                            font.pointSize: 9
                            Layout.fillWidth: true
                        }
                        RowLayout {
                            Button {
                                text: "Add files…"
                                enabled: !conv.busy
                                onClicked: openDialog.open()
                            }
                            Button {
                                text: "Clear list"
                                enabled: window.queue.length > 0 && !conv.busy
                                onClicked: conv.clearQueue()
                            }
                        }
                    }
                }

                GroupBox {
                    title: "Output"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    GridLayout {
                        anchors.fill: parent
                        columns: 3
                        columnSpacing: 8
                        Label { text: "Document" }
                        TextField {
                            Layout.fillWidth: true
                            placeholderText: "document.docx"
                            text: conv.outputPath
                            enabled: conv.selected >= 0
                            onEditingFinished: conv.setOutputText(text)
                            selectByMouse: true
                        }
                        Button {
                            text: "Save as…"
                            enabled: conv.selected >= 0
                            onClicked: saveDialog.open()
                        }
                        Label { text: "Folder" }
                        TextField {
                            Layout.fillWidth: true
                            readOnly: true
                            text: conv.outputFolder
                            placeholderText: "next to each PDF"
                        }
                        RowLayout {
                            spacing: 2
                            Button {
                                text: "Choose…"
                                onClicked: folderDialog.open()
                            }
                            ToolButton {
                                text: "✕"
                                visible: conv.outputFolder.length > 0
                                onClicked: conv.setOutputFolderUrl("")
                            }
                        }
                    }
                }

                GroupBox {
                    title: "Layout"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    ColumnLayout {
                        anchors.fill: parent
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["Flowing text, page by page", "Continuous text (reflow)", "Exact positions (frames)"]
                            currentIndex: conv.layoutMode
                            onActivated: (index) => conv.layoutMode = index
                        }
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: window.subtle
                            font.pointSize: 9
                            text: [
                                "Paragraphs, columns, lists, tables and equations are rebuilt with the original fonts, indents and spacing; every PDF page stays a page. Best for editing.",
                                "One flowing document: page breaks are dropped, text columns become Word columns, and paragraphs and tables split across pages are joined. Best for reusing the text.",
                                "Every text block is placed in a frame at its exact position on the page. Best visual fidelity, harder to edit."
                            ][conv.layoutMode] || ""
                        }
                    }
                }

                GroupBox {
                    title: "Content"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    ColumnLayout {
                        anchors.fill: parent
                        spacing: 0
                        CheckBox {
                            text: "Images"
                            checked: conv.extractImages
                            onToggled: conv.extractImages = checked
                        }
                        CheckBox {
                            text: "Charts and diagrams"
                            checked: conv.vectorFigures
                            enabled: conv.extractImages
                            onToggled: conv.vectorFigures = checked
                        }
                        CheckBox {
                            text: "Simple charts as editable shapes"
                            checked: conv.vectorShapes
                            enabled: conv.extractImages && conv.vectorFigures
                            onToggled: conv.vectorShapes = checked
                            leftPadding: 28
                        }
                        CheckBox {
                            text: "Detect tables"
                            checked: conv.detectTables
                            onToggled: conv.detectTables = checked
                        }
                        CheckBox {
                            text: "Embed the PDF's fonts (when their licence allows)"
                            checked: conv.embedFonts
                            onToggled: conv.embedFonts = checked
                        }
                        CheckBox {
                            text: "Keep text widths when fonts are substituted"
                            checked: conv.keepWidths
                            onToggled: conv.keepWidths = checked
                        }
                    }
                }

                GroupBox {
                    title: "Document structure"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    ColumnLayout {
                        anchors.fill: parent
                        spacing: 0
                        CheckBox {
                            text: "Headings (navigation pane)"
                            checked: conv.detectHeadings
                            onToggled: conv.detectHeadings = checked
                        }
                        CheckBox {
                            text: "Paragraph styles"
                            checked: conv.paragraphStyles
                            onToggled: conv.paragraphStyles = checked
                        }
                        CheckBox {
                            text: "Headers and footers with page numbers"
                            checked: conv.headersFooters
                            onToggled: conv.headersFooters = checked
                        }
                        CheckBox {
                            text: "Word lists and footnotes"
                            checked: conv.wordLists
                            onToggled: conv.wordLists = checked
                        }
                        CheckBox {
                            text: "Bookmarks, links and table of contents"
                            checked: conv.navigation
                            onToggled: conv.navigation = checked
                        }
                        CheckBox {
                            text: "Comments and form fields"
                            checked: conv.comments
                            onToggled: conv.comments = checked
                        }
                    }
                }

                GroupBox {
                    title: "OCR (scanned pages)"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    GridLayout {
                        anchors.fill: parent
                        columns: 2
                        columnSpacing: 10
                        Label { text: "Mode" }
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["Automatic", "Always", "Never"]
                            currentIndex: conv.ocrMode
                            onActivated: (index) => conv.ocrMode = index
                        }
                        Label { text: "Languages" }
                        TextField {
                            Layout.fillWidth: true
                            text: conv.ocrLanguage
                            placeholderText: "auto, eng or eng+deu"
                            onEditingFinished: conv.ocrLanguage = text
                            enabled: conv.ocrMode !== 2
                        }
                        Label { text: "Resolution" }
                        SpinBox {
                            Layout.fillWidth: true
                            from: 100
                            to: 600
                            stepSize: 50
                            value: conv.ocrDpi
                            editable: true
                            textFromValue: (v) => v + " dpi"
                            valueFromText: (t) => parseInt(t)
                            onValueModified: conv.ocrDpi = value
                            enabled: conv.ocrMode !== 2
                        }
                        CheckBox {
                            Layout.columnSpan: 2
                            text: "Straighten skewed or sideways scans"
                            checked: conv.straighten
                            enabled: conv.ocrMode !== 2
                            onToggled: conv.straighten = checked
                        }
                        CheckBox {
                            Layout.columnSpan: 2
                            text: "Read text in pictures (alternative text)"
                            checked: conv.pictureText
                            enabled: conv.ocrMode !== 2
                            onToggled: conv.pictureText = checked
                        }
                        Label {
                            Layout.columnSpan: 2
                            text: conv.ocrEngine
                            color: window.subtle
                            font.pointSize: 9
                        }
                    }
                }

                GroupBox {
                    title: "Pages & security"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    enabled: !conv.busy
                    GridLayout {
                        anchors.fill: parent
                        columns: 2
                        columnSpacing: 10
                        Label { text: "Pages" }
                        TextField {
                            Layout.fillWidth: true
                            placeholderText: window.queue.length > 1 ? "all (one file only)" : "all  (e.g. 1-3, 7, 10-)"
                            text: conv.pageRange
                            onEditingFinished: conv.pageRange = text
                        }
                        Label { text: "Password" }
                        TextField {
                            Layout.fillWidth: true
                            echoMode: TextInput.Password
                            placeholderText: "only for encrypted PDFs"
                            text: conv.password
                            onEditingFinished: conv.password = text
                        }
                    }
                }

                Item { Layout.fillHeight: true }
            }
        }
    }

    footer: Pane {
        padding: 10
        ColumnLayout {
            anchors.fill: parent
            spacing: 6

            ProgressBar {
                Layout.fillWidth: true
                value: conv.progress
                indeterminate: conv.busy && conv.progress === 0
            }
            RowLayout {
                Layout.fillWidth: true
                Label {
                    text: conv.status
                    elide: Text.ElideRight
                    Layout.fillWidth: true
                }
                Button {
                    text: "Open document"
                    visible: conv.lastOutput.length > 0 && !conv.busy
                    onClicked: Qt.openUrlExternally(conv.outputUrl())
                }
                Button {
                    text: "Show folder"
                    visible: conv.lastOutput.length > 0 && !conv.busy
                    onClicked: Qt.openUrlExternally(conv.outputFolderUrl())
                }
                ToolButton {
                    text: logView.visible ? "Hide log" : "Show log"
                    onClicked: logView.visible = !logView.visible
                }
            }
            ScrollView {
                id: logView
                visible: false
                Layout.fillWidth: true
                Layout.preferredHeight: 120
                TextArea {
                    readOnly: true
                    text: conv.log
                    font.family: "monospace"
                    font.pointSize: 9
                    wrapMode: TextEdit.NoWrap
                    onTextChanged: cursorPosition = length
                }
            }
        }
    }

    Popup {
        id: doneBanner
        x: (window.width - width) / 2
        y: window.height - height - 120
        padding: 14
        closePolicy: Popup.CloseOnPressOutside | Popup.CloseOnEscape
        background: Rectangle { color: window.accent; radius: 8 }
        Timer { running: doneBanner.visible; interval: 3500; onTriggered: doneBanner.close() }
        Label {
            id: doneLabel
            color: "white"
            text: "✔ Word document created"
            font.bold: true
        }
    }

    // Test hooks: PDF2WORD_SCREENSHOT=file.png saves the window and quits;
    // with PDF2WORD_AUTOCONVERT=1 the queue is converted first.
    function screenshot() {
        window.contentItem.parent.grabToImage(function(result) {
            result.saveToFile(conv.screenshotPath)
            Qt.quit()
        })
    }
    // PDF2WORD_EDIT_DEMO="text": turns on editing, replaces the first text run
    // and opens the editor on the second.
    Timer {
        running: conv.editDemo.length > 0
        interval: 2500
        onTriggered: conv.enableEditing(true)
    }
    readonly property bool paraDemo: conv.editDemo.startsWith("para:")
    readonly property bool ocrDemo: conv.editDemo === "ocr"
    readonly property bool findDemo: conv.editDemo.startsWith("find:")
    Timer {
        running: window.findDemo
        interval: 3500
        onTriggered: {
            findDialog.open()
            findField.text = conv.editDemo.substring(5)
            findDialog.search()
        }
    }
    Timer {
        running: window.ocrDemo
        interval: 4000
        onTriggered: conv.recognizeOutlined()
    }
    Timer {
        running: window.ocrDemo
        interval: 8000
        onTriggered: window.openEditor(window.textItems.length - 1)
    }
    Timer {
        running: conv.editDemo.length > 0 && !window.paraDemo && !window.ocrDemo && !window.findDemo
        interval: 4000
        onTriggered: conv.editRun(0, conv.editDemo, true, "{}")
    }
    Timer {
        running: window.paraDemo
        interval: 4000
        onTriggered: {
            window.paragraphMode = true
            var k = 0
            for (var i = 0; i < window.paragraphs.length; i++)
                if (window.paragraphs[i].lines > window.paragraphs[k].lines) k = i
            window.openEditor(k)
            paraField.text = paraField.text + " " + conv.editDemo.substring(5)
            editor.schedulePreview()
        }
    }
    Timer {
        running: conv.editDemo.length > 0 && !window.paraDemo && !window.ocrDemo && !window.findDemo
        interval: 7500
        onTriggered: {
            if (window.textItems.length > 1) {
                window.openEditor(1)
                editor.sizeTouched = true
                sizeBox.value = sizeBox.value + 4
                boldBtn.checked = true
                editor.schedulePreview()
            }
        }
    }
    Timer {
        running: conv.screenshotPath.length > 0
        interval: conv.editDemo.length > 0 ? 10500 : 6000
        onTriggered: conv.autoConvert ? conv.convert() : window.screenshot()
    }
    Connections {
        target: conv
        enabled: conv.autoConvert && conv.screenshotPath.length > 0
        function onFinished(success, message) { shotDelay.start() }
    }
    Timer {
        id: shotDelay
        interval: 1500
        onTriggered: window.screenshot()
    }
}

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
    readonly property var textItems: JSON.parse(conv.textItemsJson)

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

    // Change or delete a run of text.
    Dialog {
        id: editDialog
        property int itemIndex: -1
        property var item: ({})
        title: "Edit text"
        modal: true
        anchors.centerIn: parent
        width: Math.min(window.width - 80, 640)
        standardButtons: Dialog.Ok | Dialog.Cancel
        onOpened: { editField.forceActiveFocus(); editField.selectAll() }
        onAccepted: conv.replaceText(itemIndex, editField.text, fitBox.checked)

        ColumnLayout {
            anchors.fill: parent
            spacing: 8
            Label {
                text: (editDialog.item.font || "") + " · " + (editDialog.item.size || 0) + " pt"
                      + (editDialog.item.bold ? " · bold" : "") + (editDialog.item.italic ? " · italic" : "")
                      + (editDialog.item.invisible ? " · invisible (OCR text layer)" : "")
                color: window.subtle
            }
            TextField {
                id: editField
                Layout.fillWidth: true
                onAccepted: editDialog.accept()
            }
            CheckBox {
                id: fitBox
                text: "Keep within the original width (condense longer text)"
                checked: true
            }
            Label {
                Layout.fillWidth: true
                wrapMode: Text.WordWrap
                font.pointSize: 9
                color: window.subtle
                text: "The text keeps its font when the PDF contains the needed letters; otherwise the changed letters use a similar installed font."
            }
            Button {
                text: "Delete this text"
                onClicked: { conv.deleteText(editDialog.itemIndex); editDialog.close() }
            }
        }
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

    // Replace text throughout the document.
    Dialog {
        id: findDialog
        title: "Find and replace in the PDF"
        modal: true
        anchors.centerIn: parent
        width: Math.min(window.width - 80, 560)
        standardButtons: Dialog.Ok | Dialog.Cancel
        onOpened: findField.forceActiveFocus()
        onAccepted: conv.findReplace(findField.text, replaceField.text)

        GridLayout {
            anchors.fill: parent
            columns: 2
            columnSpacing: 8
            rowSpacing: 8
            Label { text: "Find" }
            TextField { id: findField; Layout.fillWidth: true }
            Label { text: "Replace with" }
            TextField { id: replaceField; Layout.fillWidth: true; onAccepted: findDialog.accept() }
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

                    // Text runs that can be edited.
                    Item {
                        id: editOverlay
                        visible: conv.editMode && preview.status === Image.Ready
                        x: preview.x + (preview.width - preview.paintedWidth) / 2
                        y: preview.y + (preview.height - preview.paintedHeight) / 2
                        width: preview.paintedWidth
                        height: preview.paintedHeight
                        enabled: !conv.editBusy && !conv.busy

                        // A click on empty space adds text there.
                        MouseArea {
                            anchors.fill: parent
                            cursorShape: Qt.IBeamCursor
                            onClicked: (mouse) => {
                                insertDialog.px = mouse.x / width
                                insertDialog.py = mouse.y / height
                                insertDialog.open()
                            }
                        }

                        Repeater {
                            model: window.textItems
                            delegate: Rectangle {
                                required property var modelData
                                x: modelData.x * editOverlay.width - 2
                                y: modelData.y * editOverlay.height - 2
                                width: Math.max(4, modelData.w * editOverlay.width + 4)
                                height: Math.max(4, modelData.h * editOverlay.height + 4)
                                radius: 2
                                color: textHover.containsMouse ? Qt.rgba(0.17, 0.34, 0.6, 0.18) : Qt.rgba(0.17, 0.34, 0.6, 0.05)
                                border.color: textHover.containsMouse ? window.accent : Qt.rgba(0.17, 0.34, 0.6, 0.35)
                                border.width: textHover.containsMouse ? 2 : 1

                                MouseArea {
                                    id: textHover
                                    anchors.fill: parent
                                    hoverEnabled: true
                                    cursorShape: Qt.PointingHandCursor
                                    onClicked: {
                                        editDialog.itemIndex = modelData.i
                                        editDialog.item = modelData
                                        editField.text = modelData.text
                                        editDialog.open()
                                    }
                                }
                                ToolTip.visible: textHover.containsMouse
                                ToolTip.text: modelData.font + " · " + modelData.size + " pt — click to edit"
                                ToolTip.delay: 400
                            }
                        }
                    }

                    BusyIndicator {
                        anchors.right: parent.right
                        anchors.top: parent.top
                        anchors.margins: 8
                        running: conv.editBusy
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
                    Label {
                        Layout.fillWidth: true
                        elide: Text.ElideRight
                        color: window.subtle
                        text: conv.editCount > 0
                              ? conv.editCount + (conv.editCount === 1 ? " change" : " changes") + " — converting uses the edited PDF"
                              : "Click a line to change it, or empty space to add text"
                    }
                    Button {
                        text: "Find and replace…"
                        enabled: !conv.editBusy && !conv.busy
                        onClicked: findDialog.open()
                    }
                    Button {
                        text: "Undo"
                        enabled: conv.editCount > 0 && !conv.editBusy && !conv.busy
                        onClicked: conv.undoEdit()
                    }
                    Button {
                        text: "Save PDF…"
                        enabled: conv.editCount > 0 && !conv.editBusy
                        onClicked: savePdfDialog.open()
                    }
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
    // PDF2WORD_EDIT_DEMO="text": turns on editing and replaces the first text run.
    Timer {
        running: conv.editDemo.length > 0
        interval: 2500
        onTriggered: conv.enableEditing(true)
    }
    Timer {
        running: conv.editDemo.length > 0
        interval: 4000
        onTriggered: conv.replaceText(0, conv.editDemo, true)
    }
    Timer {
        running: conv.editDemo.length > 0
        interval: 7500
        onTriggered: {
            if (window.textItems.length > 1) {
                editDialog.itemIndex = 1
                editDialog.item = window.textItems[1]
                editField.text = window.textItems[1].text
                editDialog.open()
            }
        }
    }
    Timer {
        running: conv.screenshotPath.length > 0
        interval: conv.editDemo.length > 0 ? 9000 : 6000
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

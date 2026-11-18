import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs

import com.pdf2word.app

ApplicationWindow {
    id: window
    width: 1180
    height: 800
    minimumWidth: 900
    minimumHeight: 600
    visible: true
    title: conv.inputPath.length > 0 ? "PDF to Word — " + conv.inputPath.split(/[\\/]/).pop() : "PDF to Word"

    readonly property color accent: "#2b579a"
    readonly property color subtle: palette.window.hslLightness < 0.5 ? "#9aa4b2" : "#5f6b7a"

    Converter {
        id: conv
        Component.onCompleted: openFromArguments()
        onFinished: (success, message) => {
            if (success)
                doneBanner.open()
        }
    }

    FileDialog {
        id: openDialog
        title: "Open PDF"
        nameFilters: ["PDF documents (*.pdf)", "All files (*)"]
        onAccepted: conv.openPdf(selectedFile)
    }

    FileDialog {
        id: saveDialog
        title: "Save Word document as"
        fileMode: FileDialog.SaveFile
        defaultSuffix: "docx"
        nameFilters: ["Word documents (*.docx)"]
        onAccepted: conv.setOutputUrl(selectedFile)
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
                text: conv.busy ? "Cancel" : "Convert to Word"
                highlighted: !conv.busy
                enabled: conv.busy || conv.pageCount > 0
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
            SplitView.minimumWidth: 400
            padding: 12

            ColumnLayout {
                anchors.fill: parent
                spacing: 8

                Rectangle {
                    Layout.fillWidth: true
                    Layout.fillHeight: true
                    color: palette.window.hslLightness < 0.5 ? "#2a2d33" : "#e6e9ee"
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

                    Column {
                        anchors.centerIn: parent
                        spacing: 12
                        visible: conv.pageCount === 0
                        Label {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "📄"
                            font.pixelSize: 64
                        }
                        Label {
                            anchors.horizontalCenter: parent.horizontalCenter
                            text: "Drop a PDF file here"
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
                                conv.openPdf(drop.urls[0])
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

                RowLayout {
                    Layout.alignment: Qt.AlignHCenter
                    enabled: conv.pageCount > 0
                    spacing: 6
                    Button {
                        text: "◀"
                        enabled: conv.previewPage > 1
                        onClicked: conv.showPage(conv.previewPage - 1)
                    }
                    SpinBox {
                        id: pageSpin
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
            }
        }

        // ------------------------------------------------------------ options
        ScrollView {
            id: optionsView
            SplitView.preferredWidth: 400
            SplitView.minimumWidth: 320
            contentWidth: availableWidth
            enabled: !conv.busy

            ColumnLayout {
                width: optionsView.availableWidth
                spacing: 12

                Item { Layout.preferredHeight: 4 }

                GroupBox {
                    title: "Output file"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    RowLayout {
                        anchors.fill: parent
                        TextField {
                            Layout.fillWidth: true
                            placeholderText: "document.docx"
                            text: conv.outputPath
                            onEditingFinished: conv.outputPath = text
                            selectByMouse: true
                        }
                        Button {
                            text: "Save as…"
                            onClicked: saveDialog.open()
                        }
                    }
                }

                GroupBox {
                    title: "Layout"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    ColumnLayout {
                        anchors.fill: parent
                        ComboBox {
                            Layout.fillWidth: true
                            model: ["Flowing text (editable)", "Exact positions (frames)"]
                            currentIndex: conv.layoutMode
                            onActivated: (index) => conv.layoutMode = index
                        }
                        Label {
                            Layout.fillWidth: true
                            wrapMode: Text.WordWrap
                            color: window.subtle
                            font.pointSize: 9
                            text: conv.layoutMode === 0
                                ? "Paragraphs, columns, lists and tables are rebuilt with the original fonts, indents and spacing. Best for editing."
                                : "Every text block is placed in a frame at its exact position on the page. Best visual fidelity, harder to edit."
                        }
                    }
                }

                GroupBox {
                    title: "OCR (scanned pages)"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
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
                            placeholderText: "eng or eng+deu"
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
                        Label {
                            Layout.columnSpan: 2
                            text: conv.ocrEngine
                            color: window.subtle
                            font.pointSize: 9
                        }
                    }
                }

                GroupBox {
                    title: "Content"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    ColumnLayout {
                        anchors.fill: parent
                        spacing: 0
                        CheckBox {
                            text: "Images"
                            checked: conv.extractImages
                            onToggled: conv.extractImages = checked
                        }
                        CheckBox {
                            text: "Charts and diagrams (rendered as pictures)"
                            checked: conv.vectorFigures
                            enabled: conv.extractImages
                            onToggled: conv.vectorFigures = checked
                        }
                        CheckBox {
                            text: "Detect tables"
                            checked: conv.detectTables
                            onToggled: conv.detectTables = checked
                        }
                        CheckBox {
                            text: "Heading styles (navigation pane)"
                            checked: conv.detectHeadings
                            onToggled: conv.detectHeadings = checked
                        }
                    }
                }

                GroupBox {
                    title: "Pages & security"
                    Layout.fillWidth: true
                    Layout.leftMargin: 12
                    Layout.rightMargin: 12
                    GridLayout {
                        anchors.fill: parent
                        columns: 2
                        columnSpacing: 10
                        Label { text: "Pages" }
                        TextField {
                            Layout.fillWidth: true
                            placeholderText: "all  (e.g. 1-3, 7, 10-)"
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
            color: "white"
            text: "✔ Word document created"
            font.bold: true
        }
    }
}

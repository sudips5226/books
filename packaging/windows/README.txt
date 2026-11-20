PDF to Word converter for Windows (x64)
=======================================

pdf2word-gui.exe   Desktop application: open or drag a PDF onto the window,
                   choose options, click "Convert to Word".
pdf2word.exe       Command-line version, e.g.
                     pdf2word.exe input.pdf -o output.docx
                     pdf2word.exe scan.pdf --ocr always
                     pdf2word.exe --help

Keep all files in this folder together: the programs need pdfium.dll (PDF
engine), the Qt DLLs and folders (user interface), the Tesseract DLLs and the
"tessdata" folder (OCR for scanned pages; English is included).

More OCR languages: download e.g. deu.traineddata from
https://github.com/tesseract-ocr/tessdata_fast into the "tessdata" folder and
enter "eng+deu" as OCR language.

If Windows SmartScreen warns about an unknown publisher (the program is not
code-signed), click "More info" and then "Run anyway".

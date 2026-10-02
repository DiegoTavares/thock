// Runs in the page when Thock is picked from Safari's share sheet, so the
// clip gets the title, the selection and the page as the reader saw it.
var ClipPage = function () {};

ClipPage.prototype = {
  run: function (parameters) {
    var selection = window.getSelection ? String(window.getSelection()) : "";
    var html = document.documentElement ? document.documentElement.outerHTML : "";
    if (html.length > 3000000) {
      html = "";
    }
    parameters.completionFunction({
      title: document.title,
      url: document.URL,
      selection: selection,
      html: html,
    });
  },
};

var ExtensionPreprocessingJS = new ClipPage();

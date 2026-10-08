// webkit-boxes: lays out an HTML page in WebKit and prints every element's border box as JSON.
// The layout suite compares these boxes with Auk's UI engine (GOAL.md, `layout`).
//
// usage: webkit-boxes <page.html> [--width CSS_PX] [--height CSS_PX]
//
// Prints {"viewport":{"w","h"},"boxes":[{"path","id","tag","x","y","w","h"}, …]} in document order.
// `path` is the element's position as child indexes from <body> ("0.2.1"), so pages need no ids.
// Fonts the page loads from file URLs next to it (for example Ahem) are allowed.

import AppKit
import WebKit

let script = """
(() => {
  const out = [];
  const walk = (el, path) => {
    const r = el.getBoundingClientRect();
    out.push({path, id: el.id || null, tag: el.tagName.toLowerCase(),
              x: r.left + window.scrollX, y: r.top + window.scrollY, w: r.width, h: r.height});
    [...el.children].forEach((c, i) => walk(c, path === "" ? String(i) : path + "." + i));
  };
  walk(document.body, "");
  return JSON.stringify(out);
})()
"""

final class Loader: NSObject, WKNavigationDelegate {
    let view: WKWebView
    var done: (Result<String, Error>) -> Void = { _ in }

    init(width: CGFloat, height: CGFloat) {
        let config = WKWebViewConfiguration()
        config.suppressesIncrementalRendering = true
        view = WKWebView(frame: NSRect(x: 0, y: 0, width: width, height: height), configuration: config)
        super.init()
        view.navigationDelegate = self
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        // Wait for web fonts before measuring.
        webView.evaluateJavaScript("document.fonts.ready.then(() => true)") { _, _ in
            webView.evaluateJavaScript(script) { value, error in
                if let error { self.done(.failure(error)) } else { self.done(.success(value as? String ?? "[]")) }
            }
        }
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        done(.failure(error))
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        done(.failure(error))
    }
}

var args = Array(CommandLine.arguments.dropFirst())
var width: CGFloat = 800
var height: CGFloat = 600
var page: String?
while !args.isEmpty {
    let a = args.removeFirst()
    switch a {
    case "--width": width = CGFloat(Double(args.removeFirst()) ?? 800)
    case "--height": height = CGFloat(Double(args.removeFirst()) ?? 600)
    default: page = a
    }
}
guard let page else {
    FileHandle.standardError.write("usage: webkit-boxes <page.html> [--width W] [--height H]\n".data(using: .utf8)!)
    exit(2)
}

let app = NSApplication.shared
app.setActivationPolicy(.prohibited)
let url = URL(fileURLWithPath: page).standardizedFileURL
let loader = Loader(width: width, height: height)
loader.done = { result in
    switch result {
    case .success(let boxes):
        print("{\"viewport\":{\"w\":\(Double(width)),\"h\":\(Double(height))},\"boxes\":\(boxes)}")
        exit(0)
    case .failure(let error):
        FileHandle.standardError.write("webkit-boxes: \(error)\n".data(using: .utf8)!)
        exit(1)
    }
}
loader.view.loadFileURL(url, allowingReadAccessTo: url.deletingLastPathComponent())
DispatchQueue.main.asyncAfter(deadline: .now() + 20) {
    FileHandle.standardError.write("webkit-boxes: timed out\n".data(using: .utf8)!)
    exit(1)
}
app.run()

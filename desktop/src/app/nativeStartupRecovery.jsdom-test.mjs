import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";
import { JSDOM } from "jsdom";

const script = await readFile(
  new URL("./nativeStartupRecovery.js", import.meta.url),
  "utf8",
);

function evaluate(markup, action = "install") {
  const dom = new JSDOM(markup, {
    runScripts: "outside-only",
    url: "http://localhost/",
  });
  dom.window.__buzzStartupRecoveryAction = action;
  dom.window.eval(script);
  return dom;
}

test("native startup fallback gives an empty root a retry screen", () => {
  const dom = evaluate('<div id="root"></div>');
  const alert = dom.window.document.querySelector(
    '#buzz-native-startup-recovery[role="alert"]',
  );

  assert.ok(alert);
  assert.match(alert.textContent, /taking longer than expected to open/);
  assert.match(alert.textContent, /local data have not been cleared/);
  assert.equal(alert.querySelector("button")?.textContent, "Reload Buzz");
});

test("native timeout fallback covers a partially mounted root without deleting it", () => {
  const dom = evaluate('<div id="root"><main>Buzz home</main></div>');

  assert.ok(dom.window.document.getElementById("buzz-native-startup-recovery"));
  assert.equal(
    dom.window.document.querySelector("#root main")?.textContent,
    "Buzz home",
  );
});

test("a late React commit can remove the temporary fallback", () => {
  const dom = evaluate('<div id="root"></div>');
  dom.window.__buzzStartupRecoveryAction = "remove";
  dom.window.eval(script);

  assert.equal(
    dom.window.document.getElementById("buzz-native-startup-recovery"),
    null,
  );
});

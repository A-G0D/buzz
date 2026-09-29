import assert from "node:assert/strict";
import test from "node:test";

import { renderStartupFailure } from "./startupFailure.ts";

test("pre-render failures show a generic accessible retry screen", () => {
  document.body.innerHTML = '<div id="root"><p>partial boot</p></div>';
  let reloadCount = 0;

  assert.equal(
    renderStartupFailure(document, () => {
      reloadCount += 1;
    }),
    true,
  );

  const root = document.getElementById("root");
  assert.equal(root?.getAttribute("role"), null);
  assert.equal(
    root
      ?.querySelector('[role="alert"]')
      ?.textContent?.includes("Buzz couldn't finish starting"),
    true,
  );
  assert.equal(root?.textContent?.includes("partial boot"), false);

  const button = root?.querySelector("button");
  assert.equal(button?.textContent, "Reload Buzz");
  button?.click();
  assert.equal(reloadCount, 1);
});

test("pre-render failure screen safely handles a missing app root", () => {
  document.body.innerHTML = "";
  assert.equal(
    renderStartupFailure(document, () => {
      assert.fail("missing root must not run the reload handler");
    }),
    false,
  );
});

import assert from "node:assert/strict";
import { afterEach, test } from "node:test";

import React, { act } from "react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import {
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import {
  createMemoryHistory,
  createRootRoute,
  createRoute,
  createRouter,
  RouterProvider,
} from "@tanstack/react-router";

import { parseCriticTriggerIntent } from "@/features/agents/ui/criticTriggerIntent";
import { CommunitiesProvider } from "@/features/communities/useCommunities";
import { MessageComposer } from "./MessageComposer.tsx";
import { ThemeProvider } from "@/shared/theme/ThemeProvider";
import { TooltipProvider } from "@/shared/ui/tooltip";

const tauriHandlers = new Map();
const tauriMock = {
  invoke(command, args) {
    const handler = tauriHandlers.get(command);
    if (handler) return handler(args);
    return Promise.reject(new Error(`Unexpected Tauri command: ${command}`));
  },
  transformCallback() {
    return 1;
  },
};
globalThis.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.__TAURI_INTERNALS__ = tauriMock;
globalThis.window.matchMedia ??= () => ({
  matches: false,
  addEventListener() {},
  removeEventListener() {},
});
globalThis.ResizeObserver ??= class {
  observe() {}
  unobserve() {}
  disconnect() {}
};
globalThis.window.ResizeObserver ??= globalThis.ResizeObserver;
globalThis.requestAnimationFrame ??= (callback) => setTimeout(callback, 0);
globalThis.cancelAnimationFrame ??= (id) => clearTimeout(id);
globalThis.window.requestAnimationFrame ??= globalThis.requestAnimationFrame;
globalThis.window.cancelAnimationFrame ??= globalThis.cancelAnimationFrame;
globalThis.Range.prototype.getClientRects ??= () => [];
globalThis.Range.prototype.getBoundingClientRect ??= () => ({
  bottom: 0,
  height: 0,
  left: 0,
  right: 0,
  top: 0,
  width: 0,
  x: 0,
  y: 0,
});

afterEach(() => {
  cleanup();
  tauriHandlers.clear();
});

async function renderComposer(props) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: Infinity } },
  });
  const content = React.createElement(
    CommunitiesProvider,
    null,
    React.createElement(
      ThemeProvider,
      null,
      React.createElement(
        TooltipProvider,
        null,
        React.createElement(
          QueryClientProvider,
          { client },
          React.createElement(MessageComposer, {
            channelName: "direct message",
            channelType: "dm",
            ...props,
          }),
        ),
      ),
    ),
  );
  const root = createRootRoute();
  const index = createRoute({
    getParentRoute: () => root,
    path: "/",
    component: () => content,
  });
  const router = createRouter({
    routeTree: root.addChildren([index]),
    history: createMemoryHistory({ initialEntries: ["/"] }),
  });
  await router.load();
  await act(async () => {
    render(React.createElement(RouterProvider, { router }));
    await new Promise((resolve) => setTimeout(resolve, 10));
  });
  return { client, router };
}

async function enterDraft(content) {
  const editor = screen.getByTestId("message-input");
  await act(async () => {
    editor.innerHTML = `<p>${content}</p>`;
    fireEvent.input(editor, { data: content, inputType: "insertText" });
    await new Promise((resolve) => setTimeout(resolve, 5));
  });
  return editor;
}

test("recognized trigger skips mention preparation and onSend, then clears draft", async () => {
  const sent = [];
  const preparedChannels = [];
  const consumedDrafts = [];
  await renderComposer({
    channelId: null,
    onConsumeSubmit(content) {
      consumedDrafts.push(content);
      return parseCriticTriggerIntent(content).status === "recognized";
    },
    onPrepareSendChannel: async () => {
      preparedChannels.push("prepared");
      return "prepared-channel";
    },
    onSend: async (...args) => sent.push(args),
  });

  const editor = await enterDraft("/critics");
  await act(async () => {
    fireEvent.submit(screen.getByTestId("message-composer"));
  });

  assert.deepEqual(consumedDrafts, ["/critics"]);
  assert.deepEqual(preparedChannels, []);
  assert.deepEqual(sent, []);
  assert.equal(editor.textContent, "");
});

test("false consume result continues normal channel preparation and send", async () => {
  const sent = [];
  const preparedChannels = [];
  await renderComposer({
    channelId: null,
    onConsumeSubmit: () => false,
    onPrepareSendChannel: async () => {
      preparedChannels.push("prepared");
      return "prepared-channel";
    },
    onSend: async (...args) => sent.push(args),
  });

  await enterDraft("ordinary reply");
  await act(async () => {
    fireEvent.submit(screen.getByTestId("message-composer"));
    await new Promise((resolve) => setTimeout(resolve, 30));
  });

  await waitFor(() => assert.equal(sent.length, 1));
  assert.deepEqual(preparedChannels, ["prepared"]);
  assert.equal(sent[0][0], "ordinary reply");
  assert.equal(sent[0][3], "prepared-channel");
});

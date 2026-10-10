import { loader } from "@monaco-editor/react";
import * as monaco from "monaco-editor/editor/editor.api.js";
import "monaco-editor/languages/definitions/python/register.js";
import EditorWorker from "monaco-editor/editor/editor.worker.js?worker";

// The workbench must stay editable without a third-party editor CDN.
const workerGlobal = globalThis as typeof globalThis & {
  MonacoEnvironment: { getWorker(): Worker };
};
workerGlobal.MonacoEnvironment = { getWorker: () => new EditorWorker() };
loader.config({ monaco });

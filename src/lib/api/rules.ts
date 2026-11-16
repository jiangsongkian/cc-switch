import { invoke } from "@tauri-apps/api/core";
import type { AppId } from "./types";

/** How rules reach an app. */
export type RuleInjection = "native" | "instructions" | "hook" | "manual";

/** Per-app enable flags. Keys match AppId (except apps without rule support). */
export interface RuleApps {
  claude: boolean;
  codex: boolean;
  gemini: boolean;
  grokbuild: boolean;
  opencode: boolean;
  hermes: boolean;
  pi: boolean;
  mcode: boolean;
  kimi: boolean;
  dsh: boolean;
  agy: boolean;
  omp: boolean;
  copilot: boolean;
  workbuddy: boolean;
}

export interface Rule {
  id: string;
  name: string;
  content: string;
  /** List summary computed by the backend content_preview. */
  preview: string;
  apps: RuleApps;
  createdAt?: number;
  updatedAt?: number;
}

export interface ImportRulesResult {
  rules: Rule[];
  /** Non-fatal issues such as rules stored but failing to project. */
  warning?: string | null;
}

export interface RuleAppSupport {
  app: AppId;
  injection: RuleInjection;
  /** Rule file extension without the dot. Currently always `md`. */
  extension: string;
  dir: string;
  displayDir: string;
  /** Whether step 3 edits the app's settings; false for native readers. */
  needsSettings: boolean;
}

/** A rule file found by Discover, taken from some app's rules directory. */
export interface DiscoverableRule {
  /** File name stem, which becomes the rule name after import. */
  name: string;
  /** First body line, used as the list summary. */
  preview: string;
  /** Apps whose rules directories contain this file. */
  apps: AppId[];
  /** A rule with the same name already exists in the database. */
  managed: boolean;
  /** Id of the matching rule when managed. */
  ruleId?: string | null;
  path: string;
  displayPath: string;
}

export const rulesApi = {
  async getRules(): Promise<Rule[]> {
    return await invoke("get_rules");
  },

  async createRule(name: string, content: string): Promise<Rule> {
    return await invoke("create_rule", { name, content });
  },

  async updateRule(id: string, name: string, content: string): Promise<Rule> {
    return await invoke("update_rule", { id, name, content });
  },

  async deleteRule(id: string): Promise<void> {
    return await invoke("delete_rule", { id });
  },

  async toggleApp(id: string, app: AppId, enabled: boolean): Promise<Rule> {
    return await invoke("toggle_rule_app", { id, app, enabled });
  },

  async syncRules(): Promise<void> {
    return await invoke("sync_rules");
  },

  async discoverRules(): Promise<DiscoverableRule[]> {
    return await invoke("discover_rules");
  },

  async importRules(names: string[]): Promise<ImportRulesResult> {
    return await invoke("import_rules", { names });
  },

  async getAppSupport(): Promise<RuleAppSupport[]> {
    return await invoke("get_rule_app_support");
  },
};

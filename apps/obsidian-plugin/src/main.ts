import { Notice, Plugin, TFile } from "obsidian";

import {
  PluginSessionClient,
} from "./companion/plugin-session-client.js";
import { proposeAfterIndexReconciliation } from "./companion/index-reconciliation.js";
import { requestPatchApproval, requestReplacement } from "./review-modals.js";
import { recordSynchronousLoad } from "./startup-metrics.js";
import {
  GrimmoreSettingTab,
  parseSettings,
  type GrimmoreSettings,
  type GrimmoreSettingsHost,
} from "./settings.js";
import { preparePatchReview, PatchReviewError } from "./vault/patch-review.js";
import { contentRevision } from "./vault/revision.js";
import {
  applyRevisionCheckedPatch,
  StaleRevisionError,
} from "./vault/vault-process-writer.js";

export default class GrimmorePlugin
  extends Plugin
  implements GrimmoreSettingsHost
{
  public override settings: GrimmoreSettings = {
    vaultId: "vault",
  };

  #client: PluginSessionClient | undefined;
  #lifecycle = new AbortController();

  public override async onload(): Promise<void> {
    const synchronousLoadStartedAt = performance.now();
    this.#lifecycle = new AbortController();
    const storedSettings = this.loadData();
    recordSynchronousLoad(synchronousLoadStartedAt);
    this.settings = parseSettings(
      (await storedSettings) as unknown,
      this.app.vault.getName(),
    );
    this.addSettingTab(new GrimmoreSettingTab(this.app, this));
    this.addCommand({
      id: "check-local-companion-health",
      name: "Check local companion health",
      callback: async () => {
        await this.#checkHealth();
      },
    });
    this.addCommand({
      id: "review-active-note-replacement",
      name: "Review a replacement for the active note",
      checkCallback: (checking) => {
        const activeFile = this.app.workspace.getActiveFile();
        const available = activeFile instanceof TFile && activeFile.extension === "md";
        if (!checking && available) {
          void this.#reviewReplacement(activeFile);
        }
        return available;
      },
    });
  }

  public override onunload(): void {
    this.#lifecycle.abort();
    this.#client?.close();
    this.#client = undefined;
  }

  public async updateSettings(settings: GrimmoreSettings): Promise<void> {
    this.settings = settings;
    this.#client?.close();
    this.#client = undefined;
    await this.saveData(settings);
  }

  async #checkHealth(): Promise<void> {
    try {
      const health = await this.#getClient().health();
      if (this.#isUnloaded()) {
        return;
      }
      if (health.status !== "ok" || health.role !== "plugin") {
        throw new Error("unexpected companion health response");
      }
      new Notice(`Grimmore companion ${health.productVersion} is ready.`);
    } catch {
      if (this.#isUnloaded()) {
        return;
      }
      new Notice(
        "Grimmore companion is unavailable. Start the installed local companion and try again.",
      );
    }
  }

  async #reviewReplacement(file: TFile): Promise<void> {
    try {
      const currentContent = await this.app.vault.read(file);
      if (this.#isUnloaded()) {
        return;
      }
      const replacement = await requestReplacement(
        this.app,
        file.path,
        currentContent,
        this.#lifecycle.signal,
      );
      if (replacement === null || this.#isUnloaded()) {
        return;
      }
      const proposal = await this.#proposeReplacement({
        path: file.path,
        expectedRevision: contentRevision(currentContent),
        replacement,
      });
      if (this.#isUnloaded()) {
        return;
      }
      const review = preparePatchReview(proposal, file.path, currentContent);
      if (
        !(await requestPatchApproval(
          this.app,
          review.unifiedDiff,
          this.#lifecycle.signal,
        )) ||
        this.#isUnloaded()
      ) {
        return;
      }
      await applyRevisionCheckedPatch(this.app.vault, file, review.proposal);
      if (!this.#isUnloaded()) {
        new Notice("Reviewed patch applied through Obsidian.");
      }
    } catch (error) {
      if (this.#isUnloaded()) {
        return;
      }
      if (error instanceof StaleRevisionError) {
        new Notice("The note changed after review. No content was written.");
      } else if (error instanceof PatchReviewError) {
        new Notice(error.message);
      } else {
        new Notice(
          "Grimmore could not prepare or apply the patch. No unreviewed write was attempted.",
        );
      }
    }
  }

  #getClient(): PluginSessionClient {
    this.#client ??= new PluginSessionClient({
      vaultId: this.settings.vaultId,
      grantId: "local",
      scopeId: "vault",
    });
    return this.#client;
  }

  async #proposeReplacement(params: {
    path: string;
    expectedRevision: string;
    replacement: string;
  }) {
    return proposeAfterIndexReconciliation(
      this.#getClient(),
      params,
      () => this.#isUnloaded(),
    );
  }

  #isUnloaded(): boolean {
    return this.#lifecycle.signal.aborted;
  }
}

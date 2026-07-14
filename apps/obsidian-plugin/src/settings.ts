import { PluginSettingTab, Setting, type App, type Plugin } from "obsidian";

export interface GrimmoreSettings {
  vaultId: string;
}

export interface GrimmoreSettingsHost extends Plugin {
  settings: GrimmoreSettings;
  updateSettings(settings: GrimmoreSettings): Promise<void>;
}

export function defaultVaultId(vaultName: string): string {
  const value = vaultName
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/gu, "-")
    .replace(/^-+|-+$/gu, "")
    .slice(0, 128);
  return value.length === 0 ? "vault" : value;
}

export function parseSettings(
  value: unknown,
  vaultName: string,
): GrimmoreSettings {
  const defaults: GrimmoreSettings = {
    vaultId: defaultVaultId(vaultName),
  };
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return defaults;
  }
  const record = value as Record<string, unknown>;
  return {
    vaultId:
      typeof record.vaultId === "string" && isPortableVaultId(record.vaultId)
        ? record.vaultId
        : defaults.vaultId,
  };
}

export class GrimmoreSettingTab extends PluginSettingTab {
  readonly #host: GrimmoreSettingsHost;

  public constructor(app: App, host: GrimmoreSettingsHost) {
    super(app, host);
    this.#host = host;
  }

  public override display(): void {
    this.containerEl.empty();
    const vaultIdSetting = new Setting(this.containerEl)
      .setName("Vault ID")
      .setDesc("Must match the vault ID configured for the local companion.");
    vaultIdSetting.addText((text) => {
      text.setValue(this.#host.settings.vaultId).onChange(async (vaultId) => {
        if (!isPortableVaultId(vaultId)) {
          vaultIdSetting.setErrorMessage(
            "Use 1–128 ASCII letters, digits, dots, dashes, or underscores.",
          );
          return;
        }
        vaultIdSetting.setErrorMessage(null);
        try {
          await this.#host.updateSettings({
            ...this.#host.settings,
            vaultId,
          });
        } catch {
          vaultIdSetting.setErrorMessage("Could not save the vault ID.");
        }
      });
    });
  }
}

function isPortableVaultId(value: string): boolean {
  return (
    value.length > 0 &&
    value.length <= 128 &&
    /^[A-Za-z0-9._-]+$/u.test(value)
  );
}

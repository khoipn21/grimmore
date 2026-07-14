import {
  Modal,
  Setting,
  TextAreaComponent,
  type App,
  type ButtonComponent,
} from "obsidian";

abstract class ResolvingModal<T> extends Modal {
  readonly #resolve: (value: T) => void;
  readonly #signal: AbortSignal;
  #settled = false;

  protected constructor(
    app: App,
    resolve: (value: T) => void,
    signal: AbortSignal,
  ) {
    super(app);
    this.#resolve = resolve;
    this.#signal = signal;
    signal.addEventListener("abort", this.#handleAbort, { once: true });
  }

  protected settle(value: T): void {
    if (!this.#settled) {
      this.#settled = true;
      this.#resolve(value);
    }
    this.close();
  }

  protected abstract cancelledValue(): T;

  public override onClose(): void {
    this.#signal.removeEventListener("abort", this.#handleAbort);
    this.contentEl.empty();
    if (!this.#settled) {
      this.#settled = true;
      this.#resolve(this.cancelledValue());
    }
  }

  readonly #handleAbort = (): void => {
    this.settle(this.cancelledValue());
  };
}

class ReplacementInputModal extends ResolvingModal<string | null> {
  readonly #path: string;
  readonly #currentContent: string;

  public constructor(
    app: App,
    path: string,
    currentContent: string,
    resolve: (value: string | null) => void,
    signal: AbortSignal,
  ) {
    super(app, resolve, signal);
    this.#path = path;
    this.#currentContent = currentContent;
  }

  protected cancelledValue(): null {
    return null;
  }

  public override onOpen(): void {
    this.setTitle("Prepare a note replacement");
    this.contentEl.createEl("p", {
      text: `Edit the complete replacement for ${this.#path}. Nothing is written until the companion validates it and you approve the diff.`,
    });
    const input = new TextAreaComponent(this.contentEl)
      .setValue(this.#currentContent)
      .setPlaceholder("Complete Markdown replacement");
    input.inputEl.addClass("grimmore-replacement-input");
    input.inputEl.setAttribute("aria-label", "Complete Markdown replacement");
    let cancelButton: ButtonComponent | undefined;
    new Setting(this.contentEl)
      .addButton((button) => {
        cancelButton = button.setButtonText("Cancel").onClick(() => {
          this.settle(null);
        });
      })
      .addButton((button) => {
        button
          .setButtonText("Review diff")
          .setCta()
          .onClick(() => {
            this.settle(input.getValue());
          });
      });
    queueMicrotask(() => {
      cancelButton?.buttonEl.focus();
    });
  }
}

class PatchApprovalModal extends ResolvingModal<boolean> {
  readonly #unifiedDiff: string;

  public constructor(
    app: App,
    unifiedDiff: string,
    resolve: (value: boolean) => void,
    signal: AbortSignal,
  ) {
    super(app, resolve, signal);
    this.#unifiedDiff = unifiedDiff;
  }

  protected cancelledValue(): false {
    return false;
  }

  public override onOpen(): void {
    this.setTitle("Review Grimmore patch");
    this.contentEl.createEl("p", {
      text: "Review every changed line. Apply writes only if the note still has the expected revision.",
    });
    const preview = this.contentEl.createEl("pre", {
      cls: "grimmore-diff-preview",
      text: this.#unifiedDiff,
    });
    preview.setAttribute("aria-label", "Complete unified patch preview");
    preview.setAttribute("tabindex", "0");
    let cancelButton: ButtonComponent | undefined;
    new Setting(this.contentEl)
      .addButton((button) => {
        cancelButton = button.setButtonText("Cancel").onClick(() => {
          this.settle(false);
        });
      })
      .addButton((button) => {
        button
          .setButtonText("Apply reviewed patch")
          .setCta()
          .onClick(() => {
            this.settle(true);
          });
      });
    queueMicrotask(() => {
      cancelButton?.buttonEl.focus();
    });
  }
}

export function requestReplacement(
  app: App,
  path: string,
  currentContent: string,
  signal: AbortSignal,
): Promise<string | null> {
  if (signal.aborted) {
    return Promise.resolve(null);
  }
  return new Promise((resolve) => {
    new ReplacementInputModal(app, path, currentContent, resolve, signal).open();
  });
}

export function requestPatchApproval(
  app: App,
  unifiedDiff: string,
  signal: AbortSignal,
): Promise<boolean> {
  if (signal.aborted) {
    return Promise.resolve(false);
  }
  return new Promise((resolve) => {
    new PatchApprovalModal(app, unifiedDiff, resolve, signal).open();
  });
}

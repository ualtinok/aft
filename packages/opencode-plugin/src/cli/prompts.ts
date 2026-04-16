import {
  confirm as clackConfirm,
  text as clackText,
  intro,
  isCancel,
  log,
  note,
  outro,
  select,
  spinner,
} from "@clack/prompts";

export { intro, log, note, outro, spinner };

function handleCancel(value: unknown): void {
  if (isCancel(value)) {
    log.warn("Setup cancelled.");
    process.exit(0);
  }
}

export async function confirm(message: string, defaultYes = true): Promise<boolean> {
  const result = await clackConfirm({
    message,
    initialValue: defaultYes,
  });
  handleCancel(result);
  return result as boolean;
}

export async function selectOne(
  message: string,
  options: { label: string; value: string; recommended?: boolean }[],
): Promise<string> {
  const result = await select({
    message,
    options: options.map((option) => ({
      label: option.recommended ? `${option.label} (recommended)` : option.label,
      value: option.value,
      hint: option.recommended ? "recommended" : undefined,
    })),
  });
  handleCancel(result);
  return result as string;
}

export async function text(
  message: string,
  options: {
    placeholder?: string;
    defaultValue?: string;
    validate?: (value: string) => string | Error | undefined;
  } = {},
): Promise<string> {
  const promptOptions: Parameters<typeof clackText>[0] = {
    message,
    ...(options.placeholder ? { placeholder: options.placeholder } : {}),
    ...(options.defaultValue !== undefined ? { defaultValue: options.defaultValue } : {}),
    ...(options.validate
      ? {
          validate: (value) => options.validate?.(value ?? ""),
        }
      : {}),
  };
  const result = await clackText(promptOptions);
  handleCancel(result);
  return result as string;
}

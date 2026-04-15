import { createInterface } from "node:readline/promises";
import { stdin as input, stdout as output } from "node:process";

const isInteractive = Boolean(input.isTTY && output.isTTY);

const ansi = {
  reset: "\u001b[0m",
  dim: "\u001b[2m",
  cyan: "\u001b[36m",
  green: "\u001b[32m",
  yellow: "\u001b[33m",
  red: "\u001b[31m",
  bold: "\u001b[1m",
};

function color(text: string, code: string): string {
  return isInteractive ? `${code}${text}${ansi.reset}` : text;
}

function prefix(kind: string, code: string): string {
  return color(kind, code);
}

async function promptLine(message: string): Promise<string> {
  const rl = createInterface({ input, output });
  try {
    return await rl.question(message);
  } finally {
    rl.close();
  }
}

export const log = {
  info(message: string): void {
    console.log(`${prefix("info", ansi.cyan)} ${message}`);
  },
  success(message: string): void {
    console.log(`${prefix("success", ansi.green)} ${message}`);
  },
  warn(message: string): void {
    console.warn(`${prefix("warn", ansi.yellow)} ${message}`);
  },
  error(message: string): void {
    console.error(`${prefix("error", ansi.red)} ${message}`);
  },
};

export function intro(message: string): void {
  console.log("");
  console.log(color(message, `${ansi.bold}${ansi.cyan}`));
}

export function outro(message: string): void {
  console.log(color(message, `${ansi.bold}${ansi.green}`));
}

export function note(message: string, title = "Note"): void {
  console.log(`${color(title, `${ansi.bold}${ansi.dim}`)}\n${message}`);
}

export function spinner() {
  let active = false;
  return {
    start(message: string) {
      active = true;
      console.log(`${prefix("…", ansi.cyan)} ${message}`);
    },
    stop(message: string) {
      if (!active) {
        console.log(`${prefix("done", ansi.green)} ${message}`);
        return;
      }
      active = false;
      console.log(`${prefix("done", ansi.green)} ${message}`);
    },
  };
}

export async function confirm(message: string, defaultYes = true): Promise<boolean> {
  if (!isInteractive) {
    return defaultYes;
  }

  const suffix = defaultYes ? " [Y/n] " : " [y/N] ";
  const answer = (await promptLine(`${message}${suffix}`)).trim().toLowerCase();
  if (!answer) {
    return defaultYes;
  }
  if (["y", "yes"].includes(answer)) {
    return true;
  }
  if (["n", "no"].includes(answer)) {
    return false;
  }
  log.warn("Unrecognized response, using default.");
  return defaultYes;
}

export async function selectOne(
  message: string,
  options: { label: string; value: string; recommended?: boolean }[],
): Promise<string> {
  if (!options.length) {
    throw new Error("selectOne requires at least one option");
  }

  if (!isInteractive) {
    const recommended = options.find((option) => option.recommended);
    return (recommended ?? options[0]).value;
  }

  console.log(message);
  options.forEach((option, index) => {
    const marker = option.recommended ? " (recommended)" : "";
    console.log(`  ${index + 1}. ${option.label}${marker}`);
  });

  while (true) {
    const answer = (await promptLine("Select an option: ")).trim();
    const choice = Number.parseInt(answer, 10);
    if (Number.isFinite(choice) && choice >= 1 && choice <= options.length) {
      return options[choice - 1].value;
    }
    log.warn("Please enter a valid option number.");
  }
}

export async function text(
  message: string,
  options: {
    placeholder?: string;
    defaultValue?: string;
    validate?: (value: string) => string | Error | undefined;
  } = {},
): Promise<string> {
  const defaultValue = options.defaultValue ?? "";
  if (!isInteractive) {
    const validation = options.validate?.(defaultValue);
    if (validation) {
      throw new Error(validation);
    }
    return defaultValue;
  }

  const hint = options.placeholder
    ? ` (${options.placeholder})`
    : defaultValue
      ? ` [default: ${defaultValue}]`
      : "";

  while (true) {
    const answer = await promptLine(`${message}${hint}: `);
    const value = answer === "" ? defaultValue : answer;
    const validation = options.validate?.(value);
    if (!validation) {
      return value;
    }
    log.warn(validation);
  }
}

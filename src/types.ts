export interface DeviceState {
  key: string;
  name: string;
  ip?: string;
  online: boolean;
}

export interface Entry {
  id: string;
  mine: boolean;
  text: string;
  at: number;
  state?: "sending" | "sent" | "delivered" | "read" | "failed";
  filePath?: string;
  read?: boolean;
}

export type History = Record<string, Entry[]>;

export function displayName(raw: string): string {
  return raw.replace(/\._lanchat\._tcp\.local\.?$/i, "");
}

export function isPreviewableImage(raw: string): boolean {
  return /\.(jpe?g|png|gif|webp|bmp)$/i.test(raw);
}

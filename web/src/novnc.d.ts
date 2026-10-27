declare module '@novnc/novnc/lib/rfb.js' {
  export default class RFB {
    constructor(
      target: HTMLElement,
      url: string,
      options?: { credentials?: { password?: string }; shared?: boolean },
    );
    scaleViewport: boolean;
    resizeSession: boolean;
    qualityLevel: number;
    compressionLevel: number;
    focusOnClick: boolean;
    viewOnly: boolean;
    addEventListener(name: string, handler: (event: Event) => void): void;
    sendKey(keysym: number, code: string, down: boolean): void;
    sendCredentials(credentials: { password: string }): void;
    focus(): void;
    disconnect(): void;
  }
}

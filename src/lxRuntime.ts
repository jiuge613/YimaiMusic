// 在 webview 内执行混淆音源脚本，取出运行时解码出的取链接口基址与协议模式。
//
// 背景：洛雪系「二次修改」聚合音源（如长青 SVIP 音源）的取链基址在脚本运行时
// 才由 JS Packer 解码拼接（静态文本里无任何 http 字面量），Rust 端无法静态解析。
// 但 webview 自带 JS 引擎——我们用一个全捕获的 Proxy 桩替换 globalThis.lx，执行
// 脚本后从所有被调用的参数里捞出 http(s) 链接，即可还原出接口地址。
//
// 仅在后端静态解析失败（报「无法解析接口地址」）时才调用本函数作为兜底，
// 避免对普通脚本也执行未知代码。

export interface LxRuntimeBase {
  /** 音源站根地址（不含尾斜杠） */
  baseUrl: string;
  /** 协议模式："" = 标准 LX 协议；"v1" = 自定义 NestJS 端点 */
  apiMode: string;
}

export function extractLxRuntimeBase(
  text: string
): LxRuntimeBase | null {
  try {
    const captured: string[] = [];
    const deepFind = (obj: unknown, depth = 0) => {
      if (depth > 25) return;
      if (typeof obj === "string") {
        if (/^https?:\/\//.test(obj)) captured.push(obj);
      } else if (Array.isArray(obj)) {
        obj.forEach((o) => deepFind(o, depth + 1));
      } else if (obj && typeof obj === "object") {
        for (const k of Object.keys(obj as Record<string, unknown>)) {
          deepFind((obj as Record<string, unknown>)[k], depth + 1);
        }
      }
    };

    const makeProxy = (): any => {
      const handler: ProxyHandler<any> = {
        get: (_t, _prop) => makeProxy(),
        apply: (_t, _this, args) => {
          args.forEach((a) => deepFind(a));
          return makeProxy();
        },
        set: () => true,
      };
      return new Proxy(function () {}, handler);
    };

    const lx = makeProxy();
    (globalThis as any).lx = lx;
    try {
      // 隔离函数作用域执行，避免污染外层变量；脚本引用的 globalThis.lx 会命中桩。
      // 末尾补一段 return：把脚本内 `const URL_CONFIG = {...}` 的值带出来
      // （const 声明不会挂到 globalThis，只能借函数返回值传出）。
      const runner = new Function(
        "lx",
        `${text}\n;return typeof URL_CONFIG!=="undefined"?URL_CONFIG:null;`
      );
      const ret = runner(lx);
      if (ret) deepFind(ret);
    } catch {
      // 执行异常（缺少浏览器 API / 运行期报错）：忽略，退回后端原始错误
    }

    if (captured.length === 0) return null;

    // 1) 优先匹配自定义 v1 端点（混淆脚本的典型特征）
    const v1 = captured.find((u) => /\/v1\/music(\/resolve-url)?/.test(u));
    if (v1) return { baseUrl: originOf(v1), apiMode: "v1" };

    // 2) 标准 LX 协议的 url.php 端点
    const std = captured.find((u) => /url\.php/.test(u));
    if (std) return { baseUrl: originOf(std), apiMode: "" };

    // 3) 兜底：取第一个不像更新/文档链接的 http(s) 地址
    const fallback = captured.find(
      (u) =>
        !/(json|changelog|update|github|kstore|raw\.github|gitee)/i.test(u)
    );
    if (fallback) return { baseUrl: originOf(fallback), apiMode: "" };

    return null;
  } catch {
    return null;
  }
}

function originOf(u: string): string {
  try {
    return new URL(u).origin;
  } catch {
    return u.replace(/\/.*$/, "").replace(/\?.*$/, "");
  }
}

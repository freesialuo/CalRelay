import { AdminFailure } from "./admin-session";
import { node, sourceForm } from "./source-form";
import { type SourceRecord, type SourcesAPI, validId } from "./sources-api";

export function mountSources(
  root: HTMLElement,
  api: SourcesAPI,
): { dispose: () => void; canLeave: () => boolean } {
  const requests = new Set<AbortController>();
  let dirty = false;
  let writing = false;
  let disposed = false;
  let selection = 0;
  let currentWrite: AbortController | undefined;
  const cancelCurrentWrite = () => {
    if (currentWrite) {
      currentWrite.abort();
      api.session.clear();
      display("写入结果未知，请读取来源核实。", "error");
    }
  };
  const status = node("p", "正在读取来源…");
  status.setAttribute("role", "status");
  const authorization = node("p");
  authorization.className = "notice";
  const list = node("div");
  const editor = node("section");
  editor.className = "panel";
  const refresh = node("button", "重新读取来源列表");
  refresh.type = "button";
  const create = node("button", "创建来源");
  create.type = "button";
  root.append(
    node("h1", "从来源开始"),
    node("p", "配置保存与事件抓取、提醒投递是不同步骤。ICS 是输入协议之一。"),
    authorization,
    status,
    refresh,
    create,
    list,
    editor,
  );
  const writes = new Set<HTMLButtonElement>([create]);
  const authorize = () => {
    authorization.textContent = api.session.writable
      ? "当前浏览器持有短期写入授权；来源与 feed token 均不提供管理权限。"
      : "当前没有可用的内存写入授权。请通过真实管理登录入口完成人机验证；仅有 Cookie 不能恢复 CSRF/nonce。未装配登录的部署暂不能写入。";
    for (const button of writes)
      button.disabled =
        !api.session.writable || writing || button.dataset.blocked === "true";
  };
  api.session.addEventListener("change", authorize);
  const display = (message: string, state: string) => {
    if (!disposed) {
      status.textContent = message;
      status.dataset.state = state;
    }
  };
  const fail = (error: unknown) =>
    display(
      error instanceof AdminFailure
        ? `${error.message}${error.requestId ? ` · 请求 ID：${error.requestId}` : ""}`
        : "输入或响应无效，请核对后重新读取。",
      "error",
    );
  const canLeave = () =>
    (!dirty && !writing) ||
    window.confirm(
      writing
        ? "写入可能已到达服务端。离开后请读取来源核实，不能当作撤回。仍要离开？"
        : "尚有未保存输入。确定离开并放弃输入？",
    );
  const run = async (operation: (signal: AbortSignal) => Promise<void>) => {
    const controller = new AbortController();
    requests.add(controller);
    try {
      await operation(controller.signal);
    } catch (error) {
      if (!disposed && !controller.signal.aborted) fail(error);
    } finally {
      requests.delete(controller);
    }
  };
  const load = async (quiet = false) => {
    if (writing) return;
    refresh.disabled = true;
    list.replaceChildren();
    if (!quiet) display("正在读取来源…", "pending");
    let after: string | undefined;
    const seen = new Set<string>();
    const appendPage = async () => {
      await run(async (signal) => {
        const page = await api.list(after, signal);
        if (disposed || signal.aborted) return;
        if (!after && page.items.length === 0)
          list.append(
            node(
              "p",
              "尚无托管来源。通过创建来源开始；旧的未托管来源不会自动迁移。",
            ),
          );
        for (const item of page.items) {
          if (seen.has(item.source_id)) continue;
          seen.add(item.source_id);
          const row = node("section");
          row.className = "panel";
          const open = node("button", "查看 / 编辑");
          open.type = "button";
          open.addEventListener("click", () => {
            if (canLeave()) {
              cancelCurrentWrite();
              const selected = ++selection;
              void run(async (requestSignal) => {
                const record = await api.read(item.source_id, requestSignal);
                if (
                  !disposed &&
                  !requestSignal.aborted &&
                  selected === selection
                )
                  edit(record);
              });
            }
          });
          row.append(
            node("h2", item.name),
            node(
              "p",
              `ID：${item.source_id} · 配置版本：${item.config_revision} · ${item.retired_at !== null ? "已退休（不可复活）" : item.enabled ? "已启用" : "已禁用"}`,
            ),
            open,
          );
          list.append(row);
        }
        list.querySelector("[data-next]")?.remove();
        if (page.next_after) {
          const next = node("button", "加载下一页");
          next.type = "button";
          next.dataset.next = "true";
          next.addEventListener("click", () => {
            next.disabled = true;
            after = page.next_after ?? undefined;
            void appendPage();
          });
          list.append(next);
        }
        if (!quiet)
          display("来源列表已读取。分页不是跨并发写入的一致快照。", "success");
      });
      refresh.disabled = false;
    };
    await appendPage();
  };
  const edit = (record?: SourceRecord) => {
    dirty = false;
    editor.replaceChildren();
    for (const button of [...writes])
      if (button !== create) writes.delete(button);
    const retired =
      record?.retired_at !== undefined && record.retired_at !== null;
    editor.append(
      node("h2", record ? `来源 ${record.source_id}` : "创建来源（先禁用）"),
    );
    const id = node("input");
    id.required = true;
    id.pattern = "[A-Za-z0-9_-]{1,128}";
    id.maxLength = 128;
    id.value = record?.source_id ?? "";
    id.readOnly = !!record;
    const idLabel = node("label", "稳定来源 ID");
    idLabel.append(id);
    const { form, read } = sourceForm(record?.configuration);
    const fields = node("fieldset");
    fields.disabled = retired;
    fields.append(idLabel, ...Array.from(form.children));
    form.append(fields);
    const save = node("button", record ? "保存配置" : "保存禁用来源");
    save.type = "submit";
    writes.add(save);
    const cancel = node("button", "取消 / 返回列表");
    cancel.type = "button";
    cancel.addEventListener("click", () => {
      if (canLeave()) {
        selection++;
        cancelCurrentWrite();
        dirty = false;
        editor.replaceChildren();
      }
    });
    const verify = node("button", "读取最新版本（保留输入）");
    verify.type = "button";
    const compare = node("div");
    let blocked = retired;
    const localWrites = new Set<HTMLButtonElement>([save]);
    let inFlight: AbortController | undefined;
    const cancelRequest = node("button", "取消等待并核实结果");
    cancelRequest.type = "button";
    cancelRequest.hidden = true;
    cancelRequest.addEventListener("click", () => {
      inFlight?.abort();
      api.session.clear();
      blocked = true;
      display(
        "写入结果未知：取消等待不等于撤回。请重新登录并读取该来源核实，勿重复提交。",
        "error",
      );
    });
    const write = async (
      operation: (sourceId: string, signal: AbortSignal) => Promise<void>,
    ) => {
      if (writing || blocked || !form.reportValidity() || !validId(id.value))
        return;
      if (!api.session.writable) {
        api.session.clear();
        display("写入授权已过期，请重新登录。", "error");
        return;
      }
      const controller = new AbortController();
      inFlight = controller;
      currentWrite = controller;
      requests.add(controller);
      writing = true;
      fields.disabled = true;
      verify.disabled = true;
      cancelRequest.hidden = false;
      authorize();
      display("写入中，请等待服务端回执…", "pending");
      let confirmed = false;
      try {
        await operation(id.value, controller.signal);
        confirmed = !controller.signal.aborted;
      } catch (error) {
        if (!disposed && !controller.signal.aborted) {
          if (
            !(error instanceof AdminFailure) ||
            ["unknown", "conflict", "denied"].includes(error.kind)
          )
            blocked = true;
          fail(error);
        }
      } finally {
        requests.delete(controller);
        writing = false;
        inFlight = undefined;
        if (currentWrite === controller) currentWrite = undefined;
        verify.disabled = false;
        cancelRequest.hidden = true;
        fields.disabled = blocked || retired;
        for (const button of localWrites)
          button.dataset.blocked = String(blocked);
        authorize();
        if (confirmed && !disposed) void load(true);
      }
    };
    form.addEventListener("input", () => {
      dirty = true;
    });
    form.addEventListener("submit", (event) => {
      event.preventDefault();
      if (blocked) return;
      let config: ReturnType<typeof read>;
      try {
        config = read();
      } catch {
        display("日历映射存在重复 ID，请修正输入。", "error");
        return;
      }
      void write(async (sourceId, signal) => {
        const saved = await api.save(
          sourceId,
          config,
          record?.config_revision ?? null,
          signal,
        );
        if (!disposed && !signal.aborted) {
          dirty = false;
          edit(saved);
          display(
            `服务端已确认保存配置版本 ${saved.config_revision}；尚未证明已抓取事件或发送提醒。`,
            "success",
          );
        }
      });
    });
    verify.addEventListener("click", () => {
      if (writing || !validId(id.value)) return;
      void run(async (signal) => {
        const latest = await api.read(id.value, signal);
        if (disposed || signal.aborted) return;
        compare.replaceChildren(
          node(
            "p",
            `当前服务端：${latest.configuration.name} · 版本 ${latest.config_revision} · ${latest.retired_at === null ? "未退休" : "已永久退休"}。原输入尚未覆盖。`,
          ),
        );
        const adopt = node("button", "采用最新版本重新编辑（放弃原输入）");
        adopt.type = "button";
        adopt.addEventListener("click", () => {
          if (canLeave()) edit(latest);
        });
        compare.append(adopt);
        display("已读取当前来源，请比较后显式采用最新版本。", "success");
      });
    });
    form.append(save, cancel, verify, cancelRequest, compare);
    if (record && !retired) {
      const retire = node("button", "退休来源");
      retire.type = "button";
      writes.add(retire);
      localWrites.add(retire);
      const confirmation = node("div");
      confirmation.hidden = true;
      const confirm = node("button", "确认永久退休");
      confirm.type = "button";
      writes.add(confirm);
      localWrites.add(confirm);
      const back = node("button", "保留来源");
      back.type = "button";
      back.addEventListener("click", () => {
        confirmation.hidden = true;
      });
      confirmation.append(
        node(
          "p",
          "永久退休会撤销关联机器 token / 单源订阅并作废未来任务，保留历史；同一 ID 不可复活。已发往外部的请求不能原子撤回。",
        ),
        confirm,
        back,
      );
      retire.addEventListener("click", () => {
        if (!blocked && !writing) confirmation.hidden = false;
      });
      confirm.addEventListener("click", () => {
        void write(async (sourceId, signal) => {
          await api.retire(sourceId, record.config_revision, signal);
          if (!disposed && !signal.aborted) {
            blocked = true;
            dirty = false;
            fields.disabled = true;
            save.disabled = retire.disabled = confirm.disabled = true;
            display(
              "服务端已确认永久退休；同一 ID 不可复活。可重新读取保留的历史记录。",
              "success",
            );
          }
        });
      });
      form.append(retire, confirmation);
    }
    if (retired)
      editor.append(
        node(
          "p",
          "此来源已永久退休，只读保留；不能启用、更新或以同一 ID 重建。",
        ),
      );
    editor.append(form);
    authorize();
    for (const button of localWrites) button.dataset.blocked = String(blocked);
    authorize();
  };
  create.addEventListener("click", () => {
    if (!writing && canLeave()) {
      selection++;
      cancelCurrentWrite();
      edit();
    }
  });
  refresh.addEventListener("click", () => {
    void load();
  });
  const unload = (event: BeforeUnloadEvent) => {
    if (dirty || writing) event.preventDefault();
  };
  window.addEventListener("beforeunload", unload);
  authorize();
  void load();
  return {
    canLeave,
    dispose: () => {
      disposed = true;
      for (const controller of requests) controller.abort();
      if (writing) api.session.clear();
      api.session.removeEventListener("change", authorize);
      window.removeEventListener("beforeunload", unload);
    },
  };
}

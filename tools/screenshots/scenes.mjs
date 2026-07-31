// Every screenshot the README needs, driven through the app's own event
// handlers rather than by poking at the DOM — so what is photographed is what
// the real backend would produce.
const wait = (ms) => new Promise((r) => setTimeout(r, ms));

export default async function ({ open, evaluate, shot }) {
  const fire = (name, payload) =>
    evaluate(`window.__fire(${JSON.stringify(name)}, ${JSON.stringify(payload)})`);
  // Setting data-pixy-state by hand only recolours the sprite — the pill's
  // title and subtitle come from PIXY_META via setPixyState(), so poking the
  // attribute produced six identical screenshots. Importing the module by the
  // same URL index.html used returns the LIVE instance, not a second copy.
  const pose = (state) =>
    evaluate(`import("/shared/pixy/pixy.js").then((m) => {
      m.setPixyState(${JSON.stringify(state)});
      return document.getElementById("mascot").dataset.pixyState;
    })`);
  // The music panel is a CLASS on #mascot, not content inside #notice — so
  // clearing the notice leaves it open and every later shot photographs it
  // instead of the card it was meant to. (It did, until this line existed.)
  //
  // The notice LOCK is module-private (notice.js) and only closePinnedCard()
  // releases it — which is bound to #card-close-btn. So the honest way to clear
  // the screen between shots is to press the same button a user would, several
  // times, because closing one card hands the screen to whatever queued behind
  // it. Setting body.className alone leaves the lock on and every later notice
  // is silently dropped into the queue instead of rendering.
  const reset = async () => {
    await evaluate(`(() => {
      const btn = document.getElementById("card-close-btn");
      for (let i = 0; i < 6 && btn; i++) btn.click();
      document.body.className = "state-idle";
      document.getElementById("notice").innerHTML = "";
      document.getElementById("quick-menu").classList.remove("visible");
      document.getElementById("mascot").classList.remove("spotify-hover");
    })()`);
    await wait(250); // let the panel's collapse transition finish
  };

  // ---- the pill, in its everyday moods -------------------------------------
  await open("mascot/index.html");
  await wait(400);

  for (const [name, state] of [
    ["idle", "idle"],
    ["coding", "coding"],
    ["thinking", "thinking"],
    ["listening", "hearing"],
    ["happy", "happy"],
    ["alert", "alert"],
  ]) {
    await reset();
    await pose(state);
    await wait(350); // the sprite animates at 10fps; let it land on a frame
    await shot(`pill-${name}`, "#mascot", { pad: 6 });
  }

  // ---- quick menu (right click) --------------------------------------------
  await reset();
  await evaluate(`document.getElementById("quick-menu").classList.add("visible")`);
  await wait(600);
  await shot("quick-menu", ["#mascot", ".quick-menu-item"], { pad: 10 });

  // ---- music panel (single click) ------------------------------------------
  await reset();
  await evaluate(`document.getElementById("mascot").click()`);
  await wait(700); // 300ms single-click timer + panel transition
  await shot("spotify", "#mascot", { pad: 6 });

  // ---- Claude Code: a command waiting for approval -------------------------
  await reset();
  await fire("mascot-permission-request", {
    session_id: null,
    request_id: "shot-1",
    tool_name: "Bash",
    tool_input: {
      command: "cargo test --workspace",
      description: "Run the backend test suite",
    },
    questions: null,
  });
  await wait(400);
  await shot("claude-permission-bash", "#mascot", { pad: 6 });

  // ---- Claude Code: an edit, with its diff ---------------------------------
  await reset();
  await fire("mascot-permission-request", {
    session_id: null,
    request_id: "shot-2",
    tool_name: "Edit",
    tool_input: {
      file_path: "src-tauri/src/ai/recall.rs",
      old_string: "const DEFAULT_MIN_COVERAGE: f32 = 0.5;",
      new_string: "const DEFAULT_MIN_COVERAGE: f32 = 0.6;",
    },
    questions: null,
  });
  await wait(400);
  await shot("claude-permission-edit", "#mascot", { pad: 6 });

  // ---- Claude Code: a question ---------------------------------------------
  await reset();
  await fire("mascot-permission-request", {
    session_id: null,
    request_id: "shot-3",
    tool_name: "AskUserQuestion",
    tool_input: {},
    questions: [
      {
        question: "Which database should the event log use?",
        header: "Storage",
        multiSelect: false,
        options: [
          { label: "SQLite" },
          { label: "A flat JSON file" },
          { label: "Postgres" },
        ],
      },
    ],
  });
  await wait(400);
  await shot("claude-question", "#mascot", { pad: 6 });

  // ---- new mail -------------------------------------------------------------
  await reset();
  await fire("mail-new", {
    from: "Nadia Rahman",
    subject: "Re: pixel sprite palette",
    description: "Asks whether the idle blue should also drive the tray icon.",
    is_reply_to_me: true,
    url: "https://mail.google.com/",
    collapsed_count: 0,
    account: "you@gmail.com",
  });
  await wait(400);
  await shot("mail-new", "#mascot", { pad: 6 });

  // ---- a meeting about to start --------------------------------------------
  await reset();
  await fire("calendar-soon", {
    title: "Design review",
    clock: "14:30",
    starts_in_minutes: 10,
    location: "Meet",
    url: "https://meet.google.com/",
    account: "you@gmail.com",
  });
  await wait(400);
  await shot("calendar-soon", "#mascot", { pad: 6 });

  // ---- GitHub: an issue assigned to you ------------------------------------
  await reset();
  await fire("github-issue-update", {
    kind: "assigned",
    title: "Wake word misfires on the word 'pixel'",
    number: 42,
    url: "https://github.com/erberkk/pixy/issues/42",
    detail: "assigned to you",
    repo: "erberkk/pixy",
  });
  await wait(400);
  await shot("github-issue", "#mascot", { pad: 6 });

  // ---- GitHub: the daily digest --------------------------------------------
  await reset();
  await fire("github-digest", {
    summary: [
      "Workload: 3 open pull requests, 1 waiting on you",
      "Attention: CI is red on main since 09:14",
      "Attention: #42 was assigned to you an hour ago",
      "Merged: #38 pin DNS between the check and the connection",
    ].join("\n"),
  });
  await wait(500);
  await shot("github-digest", "#mascot", { pad: 6 });

  // ---- the morning brief ----------------------------------------------------
  await reset();
  await fire("daily-brief", {
    unread_count: 6,
    days: 1,
    items: [
      {
        from: "Nadia Rahman",
        subject: "Re: pixel sprite palette",
        description: "Asks whether the idle blue should drive the tray icon too.",
        is_reply_to_me: true,
        url: "https://mail.google.com/",
        account: "you@gmail.com",
      },
      {
        from: "GitHub",
        subject: "[pixy] CI failed on main",
        description: "The Windows job failed at the clippy step.",
        is_reply_to_me: false,
        url: "https://github.com/",
        account: "you@gmail.com",
      },
    ],
    events: [
      {
        title: "Design review",
        clock: "14:30",
        location: "Meet",
        url: "https://meet.google.com/",
        account: "you@gmail.com",
      },
    ],
  });
  await wait(600);
  await shot("morning-brief", "#mascot", { pad: 6 });
}

// The workspace window — a separate page, so it gets its own navigation and
// its own invoke responses (see RESPONSES in shots.mjs).
export async function workspace({ open, evaluate, shot }) {
  const tab = async (mode) => {
    await evaluate(`document.querySelector('.mode-tab[data-mode="${mode}"]').click()`);
    await wait(700);
  };

  await open("workspace/workspace.html");
  await wait(1200);

  await tab("chat");
  await evaluate(`document.querySelector(".chat-item")?.click()`);
  await wait(900); // load_chat only runs when a conversation is opened
  await shot("workspace-chat", "body", { pad: 0, bg: { r: 12, g: 12, b: 14, a: 1 } });

  await tab("notes");
  await shot("workspace-notes", "body", { pad: 0, bg: { r: 12, g: 12, b: 14, a: 1 } });

  await tab("memory");
  await wait(1200); // the graph settles for a moment before it is worth a photo
  await shot("workspace-memory", "body", { pad: 0, bg: { r: 12, g: 12, b: 14, a: 1 } });
}

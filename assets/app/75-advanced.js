document.querySelectorAll("[data-adv]").forEach(btn => {
  btn.addEventListener("click", async () => {
    const tool = btn.dataset.adv;
    const needsPath = tool === "compare" || tool === "merge" || tool === "known-hash";
    const extra = needsPath ? $("advPath").value.trim() : "";
    if (needsPath && !extra) {
      $("advOut").hidden = false;
      $("advOut").textContent = "경로를 입력하십시오 — " +
        (tool === "known-hash" ? "sha256 목록 파일" : "비교/병합할 케이스 폴더");
      return;
    }
    if (tool === "merge") {
      const go = await askDialog({
        title: "케이스 병합",
        html: "<p>다른 케이스의 색인을 이 케이스에 병합합니다 (중복은 duplicate로 표시). 계속하시겠습니까?</p>",
        okLabel: "병합",
      });
      if (!go) return;
    }
    btn.disabled = true;
    $("advOut").hidden = false;
    $("advOut").textContent = "실행 중…";
    $("advLink").innerHTML = "";
    try {
      const res = await jpost("/api/advanced", { tool, extra });
      $("advOut").textContent = res.ok ? (res.output || "완료") : "실패: " + (res.error || "알 수 없는 오류");
      if (res.ok && res.url) {
        $("advLink").innerHTML = `<a href="/case/${esc(res.url)}" target="_blank">산출물 열기: ${esc(res.url)}</a>`;
      }
    } finally {
      // Re-enable unless loadEnv disabled it for a missing tool.
      btn.disabled = !!(btn.dataset && btn.dataset.envOff === "1");
      if ($("advOut").textContent === "실행 중…") $("advOut").textContent = "실패: 응답 없음 — 다시 시도할 수 있습니다.";
    }
  });
});


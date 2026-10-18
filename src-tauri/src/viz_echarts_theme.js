
/* OpenLeash: appended to the bundled Apache ECharts build served at
 * `lib/echarts.js` to isolated preview documents. It registers an `openleash`
 * theme read from the host theme tokens the document already carries on :root
 * (`--fg`, `--chart1`…), and makes it the default, so `echarts.init(el)` matches
 * the app without the agent restating the palette. An explicit theme argument
 * (including `null` for the stock look) still wins. */
(function () {
  var echarts = window.echarts;
  if (!echarts || echarts.__openleashTheme) return;
  echarts.__openleashTheme = true;
  var css = getComputedStyle(document.documentElement);
  var token = function (name, fallback) { return css.getPropertyValue("--" + name).trim() || fallback; };
  var fg = token("fg", "#e5e5e5"), muted = token("muted", "#9a9a9a"), border = token("border", "#3a3a3a");
  var card = token("card", "#1f1f1f"), font = token("font-sans", "system-ui, sans-serif");
  var palette = [];
  for (var i = 1; i <= 6; i++) palette.push(token("chart" + i, ["#a78bfa", "#60a5fa", "#34d399", "#fbbf24", "#f472b6", "#fb923c"][i - 1]));
  // Six host colours, then softer companions so a seventh series is not a repeat.
  palette.push("#22d3ee", "#a3e635", "#f87171", "#c084fc", "#94a3b8", "#facc15");
  var axis = function () {
    return {
      axisLine: { lineStyle: { color: border } },
      axisTick: { lineStyle: { color: border } },
      axisLabel: { color: muted },
      splitLine: { lineStyle: { color: border, opacity: 0.55 } },
      splitArea: { areaStyle: { color: ["rgba(127,127,127,0.04)", "rgba(127,127,127,0)"] } },
      nameTextStyle: { color: muted },
    };
  };
  echarts.registerTheme("openleash", {
    color: palette,
    backgroundColor: "transparent",
    textStyle: { color: fg, fontFamily: font },
    title: { textStyle: { color: fg, fontWeight: 600 }, subtextStyle: { color: muted } },
    legend: { textStyle: { color: fg }, pageTextStyle: { color: muted }, inactiveColor: border },
    tooltip: { backgroundColor: card, borderColor: border, textStyle: { color: fg }, extraCssText: "box-shadow:0 8px 24px rgba(0,0,0,.35);border-radius:8px;" },
    axisPointer: { lineStyle: { color: muted }, crossStyle: { color: muted }, label: { backgroundColor: card, color: fg } },
    categoryAxis: axis(), valueAxis: axis(), logAxis: axis(), timeAxis: axis(),
    radiusAxis: axis(), angleAxis: axis(),
    radar: { axisName: { color: muted }, axisLine: { lineStyle: { color: border } }, splitLine: { lineStyle: { color: border } } },
    dataZoom: { textStyle: { color: muted }, borderColor: border, fillerColor: "rgba(127,127,127,0.18)", handleStyle: { color: card, borderColor: muted } },
    visualMap: { textStyle: { color: fg } },
    timeline: { label: { color: muted }, lineStyle: { color: border } },
    toolbox: { iconStyle: { borderColor: muted }, emphasis: { iconStyle: { borderColor: fg } } },
    markPoint: { label: { color: fg } },
    graph: { color: palette },
  });
  var init = echarts.init;
  echarts.init = function (dom, theme, opts) {
    return init.call(this, dom, theme === undefined ? "openleash" : theme, opts);
  };
})();

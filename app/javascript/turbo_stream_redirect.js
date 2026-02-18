Turbo.StreamActions.redirect = function () {
  const url = this.getAttribute("url");
  const condition = this.getAttribute("from");

  if (condition) {
    const path = window.location.pathname;
    if (condition.endsWith("*")) {
      if (!path.startsWith(condition.slice(0, -1))) return;
    } else {
      if (path !== condition) return;
    }
  }

  Turbo.visit(url);
};

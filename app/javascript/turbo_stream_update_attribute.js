Turbo.StreamActions.update_attribute = function () {
  // Extract attributes from the turbo-stream element
  const attribute = this.getAttribute("attribute"); // e.g., "data-start-time"
  const content = this.querySelector("template").content; // Content to insert
  console.log(attribute, content.text_content);
  this.targetElements.forEach((element) => {
    console.log(element);
    element.setAttribute(attribute, content.textContent);
  });
};

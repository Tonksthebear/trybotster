# Stimulus Guide

Stimulus is a JavaScript framework that enhances server-rendered HTML with just enough JavaScript to make your application feel responsive and modern.

## Core Concepts

Stimulus has three main concepts:

1. **Controllers** - JavaScript classes that enhance HTML elements
2. **Actions** - How events trigger controller methods
3. **Targets** - Named DOM elements you can reference in your controller

Plus two additional concepts: 4. **Values** - Configuration data with automatic type conversion 5. **Classes** - CSS class names you can reference

---

## Setting Up a Controller

### 1. Create the Controller File

```javascript
// app/javascript/controllers/hello_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  connect() {
    console.log("Hello, Stimulus");
  }
}
```

### 2. Register (Auto-registered via index.js)

Controllers in `app/javascript/controllers/` are automatically registered if you're using the standard Rails 7+ setup.

### 3. Connect to HTML

```erb
<div data-controller="hello">
  This div is now enhanced by Stimulus!
</div>
```

---

## Targets

Targets let you reference specific elements within your controller's scope.

### Define Targets

```javascript
// app/javascript/controllers/slideshow_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["slide", "caption"];

  connect() {
    console.log("Slides:", this.slideTargets.length);
    console.log("Caption:", this.captionTarget.textContent);
  }

  // Check if target exists
  hasCaption() {
    return this.hasCaptionTarget;
  }
}
```

### Use in HTML

```erb
<div data-controller="slideshow">
  <div data-slideshow-target="slide">Slide 1</div>
  <div data-slideshow-target="slide">Slide 2</div>
  <div data-slideshow-target="slide">Slide 3</div>

  <p data-slideshow-target="caption">Caption text</p>
</div>
```

### Target Methods

```javascript
// Single target (throws if missing)
this.slideTarget;

// Check existence
this.hasSlideTarget; // boolean

// All targets
this.slideTargets; // array

// Find target
this.slideTargets.find((el) => el.dataset.active === "true");
```

---

## Actions

Actions connect DOM events to controller methods.

### Basic Action

```erb
<div data-controller="counter">
  <button data-action="click->counter#increment">+</button>
  <span data-counter-target="count">0</span>
</div>
```

```javascript
// app/javascript/controllers/counter_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["count"];

  increment(event) {
    const current = parseInt(this.countTarget.textContent);
    this.countTarget.textContent = current + 1;

    // Access the DOM event
    console.log("Clicked element:", event.currentTarget);
  }
}
```

### Action Syntax

```
data-action="[event->]controller#method[@window|@document]"
```

Examples:

```erb
<%# Click is the default for buttons %>
<button data-action="counter#increment">+</button>

<%# Explicit event %>
<input data-action="input->search#query">

<%# Multiple actions %>
<input data-action="focus->form#highlight blur->form#reset">

<%# Global events %>
<div data-action="resize@window->layout#adjust">

<%# Prevent default %>
<form data-action="submit->form#save:prevent">

<%# Custom event modifiers %>
<input data-action="keydown.enter->form#submit">
```

### Event Modifiers

```erb
<%# Prevent default %>
<form data-action="submit->form#save:prevent">

<%# Stop propagation %>
<button data-action="click->menu#toggle:stop">

<%# Run once %>
<button data-action="click->setup#initialize:once">

<%# Keyboard filters %>
<input data-action="keydown.enter->form#submit">
<input data-action="keydown.esc->modal#close">
<input data-action="keydown.meta+s->editor#save">
```

---

## Values

Values provide a type-safe way to pass configuration to controllers.

### Define Values

```javascript
// app/javascript/controllers/timer_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static values = {
    duration: Number, // Required type
    autoStart: { type: Boolean, default: false }, // With default
    message: String,
    data: Object,
    items: Array,
  };

  connect() {
    console.log(this.durationValue); // Access value
    console.log(this.hasMessageValue); // Check existence

    if (this.autoStartValue) {
      this.start();
    }
  }

  // Called when value changes
  durationValueChanged(value, previousValue) {
    console.log(`Duration changed from ${previousValue} to ${value}`);
  }
}
```

### Use in HTML

```erb
<div data-controller="timer"
     data-timer-duration-value="60"
     data-timer-auto-start-value="true"
     data-timer-message-value="Time's up!">
  Timer content
</div>
```

### Update Values from Controller

```javascript
increment() {
  this.durationValue = this.durationValue + 10
  // This triggers durationValueChanged callback
}
```

### Value Types

| Type      | Example                                | Notes                         |
| --------- | -------------------------------------- | ----------------------------- |
| `String`  | `"hello"`                              | Default type if not specified |
| `Number`  | `42`                                   | Parsed with `Number()`        |
| `Boolean` | `true`, `false`, `"true"`, `"1"`, `""` | Falsy values: false, 0, ""    |
| `Object`  | `{"key": "value"}`                     | Parsed as JSON                |
| `Array`   | `[1, 2, 3]`                            | Parsed as JSON                |

---

## Classes

Classes let you reference CSS class names from your controller.

### Define Classes

```javascript
// app/javascript/controllers/dropdown_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static classes = ["open", "closed"];
  static targets = ["menu"];

  toggle() {
    if (this.menuTarget.classList.contains(this.openClass)) {
      this.menuTarget.classList.remove(this.openClass);
      this.menuTarget.classList.add(this.closedClass);
    } else {
      this.menuTarget.classList.remove(this.closedClass);
      this.menuTarget.classList.add(this.openClass);
    }
  }
}
```

### Use in HTML

```erb
<div data-controller="dropdown"
     data-dropdown-open-class="block"
     data-dropdown-closed-class="hidden">

  <button data-action="dropdown#toggle">Toggle</button>

  <div data-dropdown-target="menu"
       class="hidden">
    Menu content
  </div>
</div>
```

---

## Lifecycle Callbacks

Stimulus controllers have lifecycle methods:

```javascript
export default class extends Controller {
  // Called when controller is connected to the DOM
  connect() {
    console.log("Connected!");
    this.setupEventListeners();
  }

  // Called when controller is disconnected from the DOM
  disconnect() {
    console.log("Disconnected!");
    this.cleanupEventListeners();
  }

  // Called when an element appears/disappears
  slideTargetConnected(element) {
    console.log("Slide target connected:", element);
  }

  slideTargetDisconnected(element) {
    console.log("Slide target disconnected:", element);
  }
}
```

**Important:** Always clean up in `disconnect()`:

- Remove event listeners added in `connect()`
- Clear timers/intervals
- Cancel pending requests

---

## Complete Example: Dropdown

```javascript
// app/javascript/controllers/dropdown_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["menu"];
  static classes = ["open"];
  static values = {
    closeOnClickOutside: { type: Boolean, default: true },
  };

  connect() {
    if (this.closeOnClickOutsideValue) {
      this.boundHandleClickOutside = this.handleClickOutside.bind(this);
    }
  }

  disconnect() {
    this.removeClickOutsideListener();
  }

  toggle(event) {
    event.preventDefault();

    if (this.isOpen) {
      this.close();
    } else {
      this.open();
    }
  }

  open() {
    this.menuTarget.classList.add(this.openClass);

    if (this.closeOnClickOutsideValue) {
      setTimeout(() => {
        document.addEventListener("click", this.boundHandleClickOutside);
      }, 0);
    }
  }

  close() {
    this.menuTarget.classList.remove(this.openClass);
    this.removeClickOutsideListener();
  }

  handleClickOutside(event) {
    if (!this.element.contains(event.target)) {
      this.close();
    }
  }

  removeClickOutsideListener() {
    if (this.boundHandleClickOutside) {
      document.removeEventListener("click", this.boundHandleClickOutside);
    }
  }

  get isOpen() {
    return this.menuTarget.classList.contains(this.openClass);
  }
}
```

```erb
<div data-controller="dropdown"
     data-dropdown-open-class="block"
     data-dropdown-close-on-click-outside-value="true"
     class="relative">

  <button data-action="dropdown#toggle"
          class="bg-blue-500 text-white px-4 py-2 rounded">
    Dropdown
  </button>

  <div data-dropdown-target="menu"
       class="hidden absolute mt-2 bg-white shadow-lg rounded">
    <a href="#" class="block px-4 py-2 hover:bg-gray-100">Item 1</a>
    <a href="#" class="block px-4 py-2 hover:bg-gray-100">Item 2</a>
    <a href="#" class="block px-4 py-2 hover:bg-gray-100">Item 3</a>
  </div>
</div>
```

---

## Common Patterns

### Auto-save Form

```javascript
// app/javascript/controllers/autosave_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["status"];
  static values = {
    url: String,
    delay: { type: Number, default: 1000 },
  };

  connect() {
    this.timeout = null;
  }

  disconnect() {
    clearTimeout(this.timeout);
  }

  save() {
    clearTimeout(this.timeout);

    this.timeout = setTimeout(() => {
      this.performSave();
    }, this.delayValue);
  }

  async performSave() {
    const formData = new FormData(this.element);
    this.showStatus("Saving...");

    try {
      const response = await fetch(this.urlValue, {
        method: "PATCH",
        body: formData,
        headers: {
          "X-CSRF-Token": document.querySelector("[name='csrf-token']").content,
        },
      });

      if (response.ok) {
        this.showStatus("Saved!", "success");
      } else {
        this.showStatus("Error saving", "error");
      }
    } catch (error) {
      this.showStatus("Error saving", "error");
    }
  }

  showStatus(message, type = "info") {
    if (this.hasStatusTarget) {
      this.statusTarget.textContent = message;
      this.statusTarget.className = `status-${type}`;
    }
  }
}
```

```erb
<%= form_with model: @post,
              data: {
                controller: "autosave",
                autosave_url_value: post_path(@post),
                action: "input->autosave#save"
              } do |f| %>

  <%= f.text_field :title %>
  <%= f.text_area :content %>

  <span data-autosave-target="status"></span>
<% end %>
```

### Character Counter

```javascript
// app/javascript/controllers/character_counter_controller.js
import { Controller } from "@hotwired/stimulus";

export default class extends Controller {
  static targets = ["input", "count"];
  static values = {
    max: { type: Number, default: 280 },
  };

  connect() {
    this.updateCount();
  }

  updateCount() {
    const length = this.inputTarget.value.length;
    const remaining = this.maxValue - length;

    this.countTarget.textContent = remaining;

    if (remaining < 0) {
      this.countTarget.classList.add("text-red-500");
    } else {
      this.countTarget.classList.remove("text-red-500");
    }
  }
}
```

```erb
<div data-controller="character-counter"
     data-character-counter-max-value="280">

  <%= text_area_tag :content, nil,
                    data: {
                      character_counter_target: "input",
                      action: "input->character-counter#updateCount"
                    },
                    class: "border rounded p-2 w-full" %>

  <div class="text-sm text-gray-600">
    <span data-character-counter-target="count">280</span> characters remaining
  </div>
</div>
```

---

## Best Practices

1. **Keep controllers small** - One clear responsibility per controller
2. **Clean up in disconnect()** - Remove listeners, clear timers
3. **Use values for configuration** - Not data attributes
4. **Use targets sparingly** - Don't over-target everything
5. **Bind event handlers** - If you need to remove them later
6. **Test without JavaScript** - Progressive enhancement
7. **Name actions clearly** - `toggle`, `open`, `close` not `handle`, `do`
8. **Use Turbo first** - Only add Stimulus when you need client-side interactivity

---

## Debugging

```javascript
connect() {
  console.log("Controller:", this.identifier)
  console.log("Element:", this.element)
  console.log("Targets:", this.constructor.targets)
  console.log("Values:", this.constructor.values)
}
```

Access from browser console:

```javascript
// Get controller instance
const element = document.querySelector("[data-controller='dropdown']");
const controller = this.application.getControllerForElementAndIdentifier(
  element,
  "dropdown",
);

// Call methods
controller.open();
controller.close();
```

---

## Reference

- [Stimulus Handbook](https://stimulus.hotwired.dev/handbook/introduction)
- [Stimulus Reference](https://stimulus.hotwired.dev/reference/controllers)
- Rails generators: `rails g stimulus [controller-name]`

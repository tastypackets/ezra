const CLASS_ATTR = /^className$|ClassName$/;
const JOIN_METHODS = new Set(["join", "concat", "trim"]);

function resolve(scope, name) {
  for (let current = scope; current; current = current.upper) {
    const variable = current.set.get(name);
    if (variable) return variable;
  }
  return null;
}

function describeManual(node, scope, seen = new Set()) {
  switch (node.type) {
    case "TemplateLiteral":
      return node.expressions.length ? "a template literal" : null;
    case "ConditionalExpression":
      return "a ternary";
    case "LogicalExpression":
      return "a logical expression";
    case "BinaryExpression":
      return "string concatenation";
    case "ArrayExpression":
      return "an array";
    case "CallExpression": {
      const { callee } = node;
      if (
        callee.type === "MemberExpression" &&
        callee.property.type === "Identifier" &&
        JOIN_METHODS.has(callee.property.name)
      ) {
        return `.${callee.property.name}()`;
      }
      return null;
    }
    case "TSNonNullExpression":
    case "TSAsExpression":
    case "TSSatisfiesExpression":
    case "ParenthesizedExpression":
      return describeManual(node.expression, scope, seen);
    case "Identifier": {
      const variable = resolve(scope, node.name);
      if (!variable || seen.has(variable)) return null;
      seen.add(variable);
      for (const def of variable.defs) {
        if (def.node.type === "VariableDeclarator" && def.node.init) {
          const found = describeManual(def.node.init, scope, seen);
          if (found) return `${found} (via \`${node.name}\`)`;
        }
      }
      return null;
    }
    default:
      return null;
  }
}

const classStringsViaCn = {
  meta: {
    type: "problem",
    docs: {
      description: "Compose class strings with cn(), not template literals, ternaries, or joins.",
    },
    messages: {
      manual: "Compose class strings with cn(), not {{what}}.",
      argument: "Pass cn() plain strings and conditions, not a template literal.",
    },
  },
  create(context) {
    return {
      JSXAttribute(node) {
        if (node.name.type !== "JSXIdentifier") return;
        if (!CLASS_ATTR.test(node.name.name)) return;
        if (node.value?.type !== "JSXExpressionContainer") return;
        const { expression } = node.value;
        if (expression.type === "JSXEmptyExpression") return;
        const what = describeManual(expression, context.sourceCode.getScope(node));
        if (what) {
          context.report({
            node: expression,
            messageId: "manual",
            data: { what },
          });
        }
      },
      CallExpression(node) {
        if (node.callee.type !== "Identifier" || node.callee.name !== "cn") {
          return;
        }
        for (const argument of node.arguments) {
          if (argument.type === "TemplateLiteral" && argument.expressions.length) {
            context.report({ node: argument, messageId: "argument" });
          }
        }
      },
    };
  },
};

export default {
  meta: { name: "ezra" },
  rules: { "class-strings-via-cn": classStringsViaCn },
};

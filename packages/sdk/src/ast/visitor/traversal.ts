/**
 * Canonical recursion over the serialized AST representation.
 *
 * Rust's typed AST stores some expression-bearing values inside ordinary
 * structs. Once serialized, those structs have no expression envelope, so the
 * TypeScript visitor must recurse through both object fields and every array
 * element until it reaches a recognized Expression value.
 */

import type { Expression } from '../../generated/Expression';
import { getExprData, isExpressionValue } from '../helpers';

export interface ExpressionLocation {
  parent: Expression;
  key: string;
  index: number | null;
}

interface ProcessOptions {
  onExpression: (node: Expression, location: ExpressionLocation) => Expression;
  cloneContainers: boolean;
  onExpressionArray?: (
    nodes: Expression[],
    location: ExpressionLocation,
  ) => void;
  removeArrayExpression?: (
    node: Expression,
    location: ExpressionLocation,
  ) => boolean;
}

const removed = Symbol('removed serialized AST value');

function processValue(
  value: unknown,
  location: ExpressionLocation,
  options: ProcessOptions,
  removable: boolean,
): unknown | typeof removed {
  if (value === null || value === undefined) return value;

  if (isExpressionValue(value)) {
    if (
      removable &&
      options.removeArrayExpression?.(value, location) === true
    ) {
      return removed;
    }
    return options.onExpression(value, location);
  }

  if (Array.isArray(value)) {
    if (
      value.length > 0 &&
      value.every(isExpressionValue) &&
      location.index === null &&
      options.onExpressionArray
    ) {
      options.onExpressionArray(value, location);
      return value;
    }

    let changed = options.cloneContainers;
    const processed: unknown[] = [];

    value.forEach((item, index) => {
      const nextLocation = { ...location, index };
      const nextValue = processValue(item, nextLocation, options, true);
      if (nextValue === removed) {
        changed = true;
        return;
      }
      processed.push(nextValue);
      if (nextValue !== item) changed = true;
    });

    return changed ? processed : value;
  }

  if (typeof value === 'object') {
    const object = value as Record<string, unknown>;
    const processed: Record<string, unknown> = {};
    let changed = options.cloneContainers;

    for (const [field, fieldValue] of Object.entries(object)) {
      const nextValue = processValue(fieldValue, location, options, false);
      if (nextValue === removed) {
        // Only direct array elements are removable.
        processed[field] = fieldValue;
      } else {
        processed[field] = nextValue;
        if (nextValue !== fieldValue) changed = true;
      }
    }

    return changed ? processed : value;
  }

  return value;
}

/**
 * Collect immediate expression children while preserving the legacy grouping
 * of direct Expression arrays exposed by `getChildren()`.
 */
export function collectExpressionChildren(
  parent: Expression,
): Array<{ key: string; value: Expression | Expression[] }> {
  const children: Array<{
    key: string;
    value: Expression | Expression[];
  }> = [];
  const innerData = getExprData(parent);
  if (!innerData || typeof innerData !== 'object') return children;

  for (const [key, value] of Object.entries(innerData)) {
    processValue(
      value,
      { parent, key, index: null },
      {
        cloneContainers: false,
        onExpression: (node, location) => {
          children.push({ key: location.key, value: node });
          return node;
        },
        onExpressionArray: (nodes, location) => {
          children.push({ key: location.key, value: nodes });
        },
      },
      false,
    );
  }

  return children;
}

/** Visit every immediate expression child below an expression payload. */
export function visitExpressionChildren(
  parent: Expression,
  visitor: (node: Expression, location: ExpressionLocation) => void,
): void {
  const innerData = getExprData(parent);
  if (!innerData || typeof innerData !== 'object') return;

  for (const [key, value] of Object.entries(innerData)) {
    processValue(
      value,
      { parent, key, index: null },
      {
        cloneContainers: false,
        onExpression: (node, location) => {
          visitor(node, location);
          return node;
        },
      },
      false,
    );
  }
}

/**
 * Rebuild an expression payload while transforming its expression children.
 * Containers are cloned so `transform(..., {})` is also a true deep clone.
 */
export function mapExpressionChildren(
  parent: Expression,
  transform: (node: Expression, location: ExpressionLocation) => Expression,
  removeArrayExpression?: (
    node: Expression,
    location: ExpressionLocation,
  ) => boolean,
): unknown {
  const innerData: unknown = getExprData(parent);
  // Unit variants have null payloads; nested enums can serialize to strings
  // (e.g. ColumnPosition::First). These leaves have no expression children.
  if (innerData === null || typeof innerData !== 'object') return innerData;
  const processed: Record<string, unknown> = {};

  for (const [key, value] of Object.entries(innerData)) {
    const nextValue = processValue(
      value,
      { parent, key, index: null },
      {
        cloneContainers: true,
        onExpression: transform,
        removeArrayExpression,
      },
      false,
    );
    processed[key] = nextValue === removed ? value : nextValue;
  }

  return processed;
}

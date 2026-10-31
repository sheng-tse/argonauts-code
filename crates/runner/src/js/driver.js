// Runs a file of steps as written. A step is a phase; `for_each = "task"` runs it once per task,
// side by side; a step that doesn't accept sends the work back; `then` names the next step;
// `until` keeps a step's agent going; `goal` goes round the whole file.
(function (send) {
  'use strict';

  const TASKS = {
    type: 'object',
    properties: {
      summary: { type: 'string' },
      tasks: {
        type: 'array',
        items: {
          type: 'object',
          properties: {
            title: { type: 'string' },
            done_when: { type: 'array', items: { type: 'string' } },
            after: { type: 'array', items: { type: 'string' } },
          },
          required: ['title'],
        },
      },
    },
    required: ['tasks'],
  };
  const VERDICT = {
    type: 'object',
    properties: {
      accept: { type: 'boolean' },
      findings: { type: 'array', items: { type: 'string' } },
    },
    required: ['accept'],
  };
  const PICK = {
    type: 'object',
    properties: { keep: { type: 'integer' }, why: { type: 'string' } },
    required: ['keep'],
  };
  const ASKS = {
    main: 'Do this.',
    orchestrator: 'Plan the work for this request.',
    worker: 'Do the work.',
    reviewer: 'Review the change. Accept it, or list what must change.',
    judge: 'Check the result against what done means.',
  };
  // Enough of an earlier answer to work from, without one long answer crowding out the rest.
  const SHOWN = 6000;
  // A loop that sends work back with no max_rounds of its own stops after this many returns,
  // the same count at which the policy stops a task sent back twice.
  const SEND_BACKS = 2;

  // A row's label is `step:task`, at most this many characters, as every label in a run is
  // (argonauts_core::look::labels::MAX_CELLS): a task's name stays whole, and a view cuts it.
  const WIDTH = 40;
  const words = (text, most) =>
    String(text).toLowerCase().split(/[^a-z0-9]+/).filter(Boolean).slice(0, most).join('-');
  const slug = (title) => words(title, 3) || 'task';
  const labelOf = (step, task, nth, of) => {
    const base = words(step.name, 2).slice(0, 12) || 'step';
    const numbered = of > 1 ? ':' + (nth + 1) : '';
    if (!task) return base + numbered;
    const room = Math.max(3, WIDTH - base.length - 1 - numbered.length);
    return base + ':' + task.id.slice(0, room).replace(/-+$/, '') + numbered;
  };
  const shown = (value) => {
    const said = typeof value === 'string' ? value : JSON.stringify(value);
    return said.length > SHOWN ? said.slice(0, SHOWN) + ' …' : said;
  };
  const bullets = (items) => items.map((item) => '- ' + item).join('\n');

  async function steps(file, args) {
    const all = file.steps;
    const request = String((args && args.request) || '');
    const crew = (args && args.crew) || {};
    const fixed = args && Array.isArray(args.tasks) && args.tasks.length ? args.tasks : null;
    const place = new Map(all.map((step, index) => [step.name, index]));
    // What the whole-flow steps said, newest last, for the steps after them.
    let said = [];
    let tasks = [];
    const notes = [];
    const runs = new Map();
    const ran = (index, task) => {
      const key = index + ':' + (task ? task.id : '');
      const times = (runs.get(key) || 0) + 1;
      runs.set(key, times);
      return times;
    };

    const name = (list) => {
      const used = new Set();
      return list.map((task, index) => {
        const given = typeof task === 'string' ? { title: task } : task || {};
        const title = String(given.title || 'task ' + (index + 1));
        let id = slug(title);
        for (let n = 2; used.has(id); n++) id = slug(title) + '-' + n;
        used.add(id);
        return {
          id,
          title,
          done_when: Array.isArray(given.done_when) ? given.done_when : [],
          after: Array.isArray(given.after) ? given.after.map(String) : [],
        };
      });
    };
    if (fixed) tasks = name(fixed);

    const fill = (words, task) =>
      words.replaceAll('{request}', request).replaceAll('{task}', task ? task.title : request);

    const prompt = (step, task, feedback, isPlanner) => {
      const parts = [fill(step.does || ASKS[step.role], task)];
      if (isPlanner && fixed) {
        parts.push('The tasks are fixed. Plan these and no others:\n' + bullets(tasks.map((t) => t.title)));
      } else if (isPlanner) {
        parts.push(
          'List the tasks, each with a short title and what done means for it. The tasks run side ' +
            'by side from the same start, so each stands alone: its own files, its own tests, and ' +
            'no task that only writes a note. A task that needs another one finished first names ' +
            "that task's title in after, and waits for it.",
        );
      }
      if (request && !(step.does || '').includes('{request}')) {
        // A step about one task reads the request for context and judges that task alone.
        parts.push(task ? 'The request, of which this task is one part:\n' + request : 'The request:\n' + request);
      }
      if (task) {
        const done = task.done_when.length ? '\nDone when:\n' + bullets(task.done_when) : '';
        parts.push('The task: ' + task.title + done);
      }
      if (!step.independent) {
        for (const earlier of said) parts.push('From ' + earlier.step + ':\n' + earlier.text);
      }
      if (feedback) parts.push(feedback);
      return parts.join('\n\n');
    };

    const count = (step) => {
      if (step.count === 'all') return Math.max(1, Number(crew[step.role]) || 1);
      return Math.max(1, Number(step.count) || 1);
    };

    // One agent of a step, kept going while the step's `until` doesn't hold.
    async function one(step, index, task, nth, of, feedback, round, schema, isPlanner) {
      const label = labelOf(step, task, nth, of);
      let pressed = feedback;
      for (let tryNumber = 1; ; tryNumber++) {
        const answer = await agent(prompt(step, task, pressed, isPlanner), {
          role: step.role,
          label,
          phase: step.name,
          schema,
          // A step that names a model runs on it, and otherwise each agent on one of the role's.
          model: step.model || nth,
          task: task ? task.id : undefined,
          // The tasks it builds on, whose accepted work Argonauts merges into the run first.
          after: task && task.builds_on && task.builds_on.length ? task.builds_on : undefined,
          // Until the tasks are listed, workers think and write plans. A step that judges reads.
          access: step.send_back_to || (file.planner !== null && index <= file.planner) ? 'read' : undefined,
          // Several workers on one piece of work each need a worktree of their own.
          isolation: step.tries ? 'worktree' : undefined,
          tryNumber: (round - 1) + tryNumber,
          independent: step.independent || undefined,
          notTheAuthor: step.not_the_author || undefined,
          keptBy: step.kept_by,
          until: step.until ? fill(step.until, task) : undefined,
          required: step.required || undefined,
          ownWorkflow: step.own_workflow || undefined,
        });
        if (!step.until || answer === null) return answer;
        const goal = fill(step.until, task);
        const checked = await check(goal, {
          step: step.name,
          task: task ? task.id : undefined,
          label,
          keptBy: step.kept_by,
          tryNumber,
        });
        if (!checked || checked.holds) return answer;
        if (checked.stop || (step.max_rounds && tryNumber >= step.max_rounds)) {
          notes.push(`${label} stopped before "${goal}" held`);
          log(`${label}: stopped before "${goal}" held`);
          return answer;
        }
        pressed = `Not true yet: ${goal}` + (checked.failed ? '\n' + checked.failed : '');
      }
    }

    // A step once, for one task or for the whole. Says where the flow goes next.
    async function run(index, task, feedback) {
      const step = all[index];
      const round = ran(index, task);
      if (step.max_rounds && !step.until && round > step.max_rounds) {
        const what = (task ? task.title + ': ' : '') + step.name;
        notes.push(`${what} stopped after ${step.max_rounds} rounds without acceptance`);
        log(`${what}: stopped after ${step.max_rounds} rounds without acceptance`);
        return { gaveUp: true };
      }
      const isPlanner = index === file.planner && !task;
      const earlier = index > 0 ? all[index - 1] : null;
      const picks = step.role === 'judge' && !step.send_back_to && earlier && earlier.tries;
      const schema = step.send_back_to ? VERDICT : isPlanner ? TASKS : picks ? PICK : undefined;
      const of = count(step);
      step.tries = step.role === 'worker' && of > 1 && !file.per_task[index] ? of : 0;
      const calls = [];
      for (let nth = 0; nth < of; nth++) {
        calls.push(() => one(step, index, task, nth, of, feedback, round, schema, isPlanner));
      }
      const answers = of === 1 ? [await calls[0]()] : await parallel(calls);
      const given = answers.filter((answer) => answer !== null && answer !== undefined);

      if (isPlanner && !fixed) {
        const listed = given.flatMap((answer) => (answer && Array.isArray(answer.tasks) ? answer.tasks : []));
        tasks = name(listed.length ? listed : [request || step.name]);
      }
      if (picks && given[0] && Number.isInteger(given[0].keep)) {
        const nth = given[0].keep - 1;
        const label = labelOf(earlier, null, nth, earlier.tries);
        send('keep', { step: earlier.name, model_index: nth, label, why: given[0].why });
      }
      if (!task) {
        const texts = given.map((answer, nth) =>
          (given.length > 1 ? `${nth + 1}) ` : '') + shown(answer && answer.summary ? answer.summary : answer));
        if (texts.length) said = said.filter((s) => s.step !== step.name).concat({ step: step.name, text: texts.join('\n') });
      }

      if (step.send_back_to) {
        const what = (task ? task.title + ': ' : '') + step.name;
        // No answer at all says nothing about the work, so building it again would only repeat.
        if (!given.length) {
          notes.push(`${what} gave no answer, so the work was neither accepted nor sent back`);
          log(`${what}: gave no answer, so the work stops here unaccepted`);
          return { gaveUp: true };
        }
        const accepted = given.every((answer) => answer && answer.accept === true);
        if (!accepted) {
          const back = place.get(step.send_back_to);
          const loop = all.slice(Math.min(back, index), Math.max(back, index) + 1);
          if (!loop.some((inside) => inside.max_rounds) && round > SEND_BACKS) {
            notes.push(`${what} stopped after sending the work back ${SEND_BACKS} times without accepting it`);
            log(`${what}: stopped after ${SEND_BACKS} send-backs without acceptance`);
            return { gaveUp: true };
          }
          const findings = given.flatMap((answer) => (answer && Array.isArray(answer.findings) ? answer.findings : []));
          const why = findings.length ? bullets(findings) : 'It was not accepted, and no reason was given.';
          return { to: back, feedback: `Sent back by ${step.name}:\n${why}`, sentBack: true };
        }
      }
      if (step.then) return { to: place.get(step.then) };
      return { result: given.length === 1 ? given[0] : given };
    }

    // The steps from `from` on, for the whole flow or for one task inside [low, high).
    async function walk(from, low, high, task, entry) {
      let at = from;
      let feedback = entry;
      let jumped = entry !== undefined;
      let last;
      let accepted = true;
      while (at >= low && at < high) {
        if (!jumped && file.only_when_sent_back[at]) {
          at++;
          continue;
        }
        if (!task && file.per_task[at]) {
          let start = at;
          let end = at;
          while (start > 0 && file.per_task[start - 1]) start--;
          while (end < all.length && file.per_task[end]) end++;
          if (!tasks.length) tasks = name([request || 'the request']);
          const entered = feedback;
          // A task waits for the tasks it names in after; one that would wait on itself in a
          // ring doesn't, so nothing waits forever.
          const begun = new Map();
          const begin = (item, ring) => {
            if (!begun.has(item.id)) {
              const path = new Set(ring).add(item.id);
              const first = item.after
                .map((named) => tasks.find((other) => other.title === named || other.id === slug(named)))
                .filter((other) => other && !path.has(other.id));
              item.builds_on = first.map((other) => other.id);
              begun.set(
                item.id,
                (async () => {
                  for (const other of first) {
                    const ended = await begin(other, path);
                    // Work that builds on a task nobody accepted would build on nothing.
                    if (!(ended && ended.accepted)) {
                      log(`${item.title}: skipped, since it comes after ${other.title}, which wasn't accepted`);
                      return { accepted: false };
                    }
                  }
                  return walk(at, start, end, item, entered);
                })(),
              );
            }
            return begun.get(item.id);
          };
          const each = await parallel(tasks.map((item) => () => begin(item, new Set())));
          const out = each.filter((ended) => ended && ended.to !== undefined);
          accepted = accepted && each.every((ended) => ended && ended.accepted);
          if (out.length) {
            at = out[0].to;
            feedback = out.map((ended) => ended.feedback).filter(Boolean).join('\n\n') || undefined;
            jumped = true;
          } else {
            const how = tasks.map((item, i) => item.title + (each[i] && each[i].accepted ? '' : ' (not accepted)'));
            said = said.filter((s) => s.step !== 'the tasks').concat({ step: 'the tasks', text: bullets(how) });
            at = end;
            feedback = undefined;
            jumped = false;
          }
          continue;
        }
        const next = await run(at, task, feedback);
        if (next.gaveUp) {
          accepted = false;
          // Past the loop: the first step below everything this one could reach.
          let below = at;
          for (const step of [all[at]].concat(all.filter((s) => s.send_back_to === all[at].name || s.then === all[at].name))) {
            below = Math.max(below, place.get(step.name));
          }
          at = below + 1;
          feedback = undefined;
          jumped = false;
          continue;
        }
        if (next.to !== undefined) {
          if (next.to < low || next.to >= high) return { to: next.to, feedback: next.feedback, accepted };
          at = next.to;
          feedback = next.feedback;
          jumped = true;
          continue;
        }
        last = next.result;
        at++;
        feedback = undefined;
        jumped = false;
      }
      return { result: last, accepted };
    }

    let ended;
    for (let round = 1; ; round++) {
      ended = await walk(0, 0, all.length, null, undefined);
      if (!file.goal) break;
      const checked = await check(file.goal, { round });
      if (!checked || checked.holds) break;
      if (checked.stop || (file.max_rounds && round >= file.max_rounds)) {
        notes.push(`stopped before the goal held: ${file.goal}`);
        ended.accepted = false;
        break;
      }
      runs.clear();
      said = [{ step: 'the goal', text: `Not true yet: ${file.goal}` + (checked.failed ? '\n' + checked.failed : '') }];
      log(`the goal doesn't hold yet, so another round: ${file.goal}`);
    }
    return {
      accepted: ended.accepted,
      tasks: tasks.map((task) => ({ id: task.id, title: task.title })),
      result: ended.result === undefined ? null : ended.result,
      notes,
    };
  }

  Object.defineProperty(globalThis, 'steps', { value: steps, writable: false, configurable: false });
})

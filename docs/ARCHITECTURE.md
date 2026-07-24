# Architecture — <Project Name>

> The system **as it stands today**: the parts it is built from, what each is responsible for, and
> how they fit together. It holds no rule and no decision. It names the ADR behind each structural
> choice; where it disagrees with an accepted ADR, the ADR is right and this document is stale. It
> is updated in the same change as the structure it describes
> ([ADR-0001](adr/0001-agent-governance-model.md)).
>
> **Kept current by:** <one named role or person>. A document everyone may edit and nobody owns is
> the one that goes stale.
>
> **Last design revision:** none yet, due after 20 changes. The revision that ran moves the
> date; the number is this project's to set; a sensor counts the changes outside `docs/` since
> the date and says when the next is due ([ADR-0004](adr/0004-feature-layer.md)).

## Context

What sits outside the system and what crosses its boundary. A diagram earns its place here more
than anywhere else, and none beats one that has stopped being true.

TODO — fill in once the system has a boundary worth drawing.

## Building blocks

The parts the system is made of, each with one responsibility, named in the vocabulary of
[`GLOSSARY.md`](GLOSSARY.md). One level deep, deeper only where the size of a part earns it. The
golden path and the test pattern an agent copies from are named here.

- **<Part>** — what it is responsible for, what it deliberately does not do, and the ADR that
  put it there.

TODO — fill in once there is more than one part.

## How it runs

The few paths worth following end to end, such as a request, a job, or a build, and where state
lives between them. Only what a newcomer would otherwise reconstruct from code.

TODO — fill in once there is more than one part to connect.

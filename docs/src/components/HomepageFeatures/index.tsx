import type {ReactNode} from 'react';
import clsx from 'clsx';
import Heading from '@theme/Heading';
import styles from './styles.module.css';

type FeatureItem = {
  title: string;
  Svg: React.ComponentType<React.ComponentProps<'svg'>>;
  description: ReactNode;
};

const FeatureList: FeatureItem[] = [
  {
    title: 'Agnostic',
    Svg: require('@site/static/img/undraw_docusaurus_mountain.svg').default,
    description: (
      <>
        Since Starthub runs on WebAssembly and Docker, it's independent of any framework or programming language. 
        You can use any language or framework that compiles to these formats.
      </>
    ),
  },
  {
    title: 'Open Source',
    Svg: require('@site/static/img/undraw_docusaurus_tree.svg').default,
    description: (
      <>
        Starthub is fully open source. The CLI and server are available on GitHub, allowing you to 
        contribute, customize, and understand how everything works under the hood.
      </>
    ),
  },
  {
    title: 'Zero Trust',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        None of your credentials are ever shared with third parties. WebAssembly and Docker ensure 
        that least-privilege principles are followed, with actions running in isolated environments 
        with explicit, minimal permissions.
      </>
    ),
  },
  {
    title: 'Encapsulated',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Actions are self-contained units with well-defined inputs and outputs. They can be composed 
        together to build complex workflows while maintaining clear boundaries and data flow.
      </>
    ),
  },
  {
    title: 'Versioned',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Every action is versioned in the registry. You can pin specific versions, track changes, 
        and ensure reproducible executions across different environments.
      </>
    ),
  },
  {
    title: 'Extensible flow-control primitives',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Define your own flow-control primitives by composing actions with wires and steps. Like Clojure's 
        extensible control structures, you can build custom control flow patterns and extend the execution 
        engine dynamically through composition.
      </>
    ),
  },
  {
    title: 'Type-checked',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Starthub validates and type-checks all inputs and outputs at runtime. Supports built-in types 
        (String, Boolean, Object, Array, Number, Any) and custom types with automatic type casting.
      </>
    ),
  },
  {
    title: 'JSON-based',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Actions are defined using JSON manifests with JSON Schema validation. All inputs, outputs, 
        and configurations use JSON, making it easy to work with and integrate into any system.
      </>
    ),
  },
  {
    title: 'Git-based',
    Svg: require('@site/static/img/undraw_docusaurus_react.svg').default,
    description: (
      <>
        Actions are stored and versioned in Git repositories. Leverage Git's powerful version control, 
        branching, and collaboration features to manage your actions alongside your code.
      </>
    ),
  },
];

function Feature({title, Svg, description}: FeatureItem) {
  return (
    <div className={clsx('col col--4')}>
      <div className="text--center">
        <Svg className={styles.featureSvg} role="img" />
      </div>
      <div className="text--center padding-horiz--md">
        <Heading as="h3">{title}</Heading>
        <p>{description}</p>
      </div>
    </div>
  );
}

export default function HomepageFeatures(): ReactNode {
  return (
    <section className={styles.features}>
      <div className="container">
        <div className="row">
          {FeatureList.map((props, idx) => (
            <Feature key={idx} {...props} />
          ))}
        </div>
      </div>
    </section>
  );
}
